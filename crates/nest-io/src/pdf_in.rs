//! Illustrator (.ai saved with PDF compatibility) and PDF reader. Interprets the vector path
//! operators of each page's content stream, including Form XObjects.

use crate::RawImport;
use anyhow::{Context, Result, bail};
use kurbo::{Affine, BezPath, Point};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

const MM_PER_PT: f64 = 25.4 / 72.0;
const MAX_FORM_DEPTH: usize = 12;

/// Where the PDF header starts, if it's in the first 1 KB (some writers put bytes before it).
fn pdf_start(data: &[u8]) -> Option<usize> {
    data[..data.len().min(1024)].windows(5).position(|w| w == b"%PDF-")
}

pub(crate) fn is_pdf(data: &[u8]) -> bool {
    pdf_start(data).is_some()
}

pub(crate) fn read(data: &[u8]) -> Result<RawImport> {
    let Some(start) = pdf_start(data) else { bail!("It isn't a PDF file.") };
    let doc = match Document::load_mem(data) {
        Err(_) if start > 0 => Document::load_mem(&data[start..]),
        r => r,
    }
    .context("The PDF data in it is damaged or uses a format this app can't read")?;
    let mut groups = Vec::new();
    let mut saw_text = false;
    let mut skipped_pages = 0;
    for (_, page_id) in doc.get_pages() {
        let content = match doc.get_and_decode_page_content(page_id) {
            Ok(c) => c,
            Err(_) => {
                skipped_pages += 1;
                continue;
            }
        };
        let resources = page_resources(&doc, page_id);
        let mut interp = Interp {
            doc: &doc,
            ctm: Affine::scale(MM_PER_PT),
            stack: Vec::new(),
            path: Vec::new(),
            current: None,
            start: Point::ZERO,
            out: Vec::new(),
            saw_text: false,
        };
        interp.run(&content, &resources, 0);
        saw_text |= interp.saw_text;
        groups.push(interp.out);
    }
    let mut warnings = Vec::new();
    if saw_text {
        warnings.push(
            "Text in {file} wasn't imported. Convert text to outlines in Illustrator first."
                .to_string(),
        );
    }
    if skipped_pages > 0 {
        warnings.push(format!(
            "Skipped {skipped_pages} unreadable page(s) in {{file}}."
        ));
    }
    Ok(RawImport { groups, warnings })
}

/// The page's resource dictionaries, nearest first (inherited ones follow).
fn page_resources(doc: &Document, page_id: ObjectId) -> Vec<Dictionary> {
    let mut out = Vec::new();
    if let Ok((direct, ids)) = doc.get_page_resources(page_id) {
        if let Some(d) = direct {
            out.push(d.clone());
        }
        for id in ids {
            if let Ok(d) = doc.get_dictionary(id) {
                out.push(d.clone());
            }
        }
    }
    out
}

struct Interp<'a> {
    doc: &'a Document,
    ctm: Affine,
    stack: Vec<Affine>,
    /// Subpaths of the path under construction, in mm, with whether each is closed.
    path: Vec<(BezPath, bool)>,
    current: Option<Point>,
    start: Point,
    out: Vec<BezPath>,
    saw_text: bool,
}

fn nums(ops: &[Object]) -> Vec<f64> {
    ops.iter()
        .filter_map(|o| o.as_float().ok())
        .map(|v| v as f64)
        .collect()
}

impl Interp<'_> {
    fn run(&mut self, content: &Content<Vec<Operation>>, resources: &[Dictionary], depth: usize) {
        for op in &content.operations {
            let n = nums(&op.operands);
            match op.operator.as_str() {
                "q" => self.stack.push(self.ctm),
                "Q" => {
                    if let Some(m) = self.stack.pop() {
                        self.ctm = m;
                    }
                }
                "cm" if n.len() == 6 => {
                    self.ctm *= Affine::new([n[0], n[1], n[2], n[3], n[4], n[5]]);
                }
                "m" if n.len() == 2 => self.move_to(Point::new(n[0], n[1])),
                "l" if n.len() == 2 => {
                    let p = self.ctm * Point::new(n[0], n[1]);
                    self.ensure_started();
                    self.cur().line_to(p);
                    self.current = Some(p);
                }
                "c" if n.len() == 6 => {
                    let (a, b, p) = (
                        self.map(n[0], n[1]),
                        self.map(n[2], n[3]),
                        self.map(n[4], n[5]),
                    );
                    self.ensure_started();
                    self.cur().curve_to(a, b, p);
                    self.current = Some(p);
                }
                "v" if n.len() == 4 => {
                    let a = self.current.unwrap_or(Point::ZERO);
                    let (b, p) = (self.map(n[0], n[1]), self.map(n[2], n[3]));
                    self.ensure_started();
                    self.cur().curve_to(a, b, p);
                    self.current = Some(p);
                }
                "y" if n.len() == 4 => {
                    let (a, p) = (self.map(n[0], n[1]), self.map(n[2], n[3]));
                    self.ensure_started();
                    self.cur().curve_to(a, p, p);
                    self.current = Some(p);
                }
                "h" => self.close(),
                "re" if n.len() == 4 => {
                    let (x, y, w, h) = (n[0], n[1], n[2], n[3]);
                    self.move_to(Point::new(x, y));
                    for p in [(x + w, y), (x + w, y + h), (x, y + h)] {
                        let p = self.map(p.0, p.1);
                        self.cur().line_to(p);
                    }
                    self.close();
                }
                // Stroke keeps open subpaths open; fills close everything.
                "S" => self.commit(false),
                "s" => {
                    self.close();
                    self.commit(false);
                }
                "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                    if matches!(op.operator.as_str(), "b" | "b*") {
                        self.close();
                    }
                    self.commit(true);
                }
                "n" => {
                    self.path.clear();
                    self.current = None;
                }
                "Tj" | "TJ" | "'" | "\"" => self.saw_text = true,
                "Do" => {
                    if let Some(Ok(name)) = op.operands.first().map(|o| o.as_name()) {
                        self.form(name, resources, depth);
                    }
                }
                _ => {}
            }
        }
    }

    fn map(&self, x: f64, y: f64) -> Point {
        self.ctm * Point::new(x, y)
    }

    fn move_to(&mut self, p: Point) {
        let p = self.ctm * p;
        let mut sub = BezPath::new();
        sub.move_to(p);
        self.path.push((sub, false));
        self.current = Some(p);
        self.start = p;
    }

    /// Drawing without a preceding `m` starts at the current point.
    fn ensure_started(&mut self) {
        if self.path.last().is_none_or(|(_, closed)| *closed) {
            let p = self.current.unwrap_or(Point::ZERO);
            let mut sub = BezPath::new();
            sub.move_to(p);
            self.path.push((sub, false));
            self.start = p;
        }
    }

    fn cur(&mut self) -> &mut BezPath {
        &mut self.path.last_mut().expect("ensure_started was called").0
    }

    fn close(&mut self) {
        if let Some((sub, closed)) = self.path.last_mut()
            && !*closed
        {
            sub.close_path();
            *closed = true;
            self.current = Some(self.start);
        }
    }

    fn commit(&mut self, close_all: bool) {
        let mut out = BezPath::new();
        for (sub, closed) in self.path.drain(..) {
            if sub.elements().len() < 2 {
                continue;
            }
            for el in sub.elements() {
                out.push(*el);
            }
            if close_all && !closed {
                out.close_path();
            }
        }
        if !out.elements().is_empty() {
            self.out.push(out);
        }
        self.current = None;
    }

    fn form(&mut self, name: &[u8], resources: &[Dictionary], depth: usize) {
        if depth >= MAX_FORM_DEPTH {
            return;
        }
        let Some(stream) = resources.iter().find_map(|r| {
            let xobjects = deref(self.doc, r.get(b"XObject").ok()?)?.as_dict().ok()?;
            deref(self.doc, xobjects.get(name).ok()?)?.as_stream().ok()
        }) else {
            return;
        };
        let is_form = stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|s| s.as_name().ok())
            .is_some_and(|s| s == b"Form");
        if !is_form {
            return;
        }
        let Ok(bytes) = stream
            .decompressed_content()
            .or_else(|_| Ok::<_, lopdf::Error>(stream.content.clone()))
        else {
            return;
        };
        let Ok(content) = Content::decode(&bytes) else {
            return;
        };
        let matrix = stream
            .dict
            .get(b"Matrix")
            .ok()
            .and_then(|m| m.as_array().ok())
            .map(|a| nums(a))
            .filter(|m| m.len() == 6)
            .map(|m| Affine::new([m[0], m[1], m[2], m[3], m[4], m[5]]))
            .unwrap_or(Affine::IDENTITY);
        let mut inner: Vec<Dictionary> = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|r| deref(self.doc, r))
            .and_then(|r| r.as_dict().ok())
            .map(|d| vec![d.clone()])
            .unwrap_or_default();
        inner.extend(resources.iter().cloned());

        let saved_ctm = self.ctm;
        let saved_stack = self.stack.len();
        self.ctm *= matrix;
        self.run(&content, &inner, depth + 1);
        self.stack.truncate(saved_stack);
        self.ctm = saved_ctm;
    }
}

fn deref<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Object> {
    match obj {
        Object::Reference(id) => doc.get_object(*id).ok(),
        o => Some(o),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::import_ai;
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    /// Builds a one-page PDF from raw content operations, with an optional Form XObject `/F1`.
    pub(crate) fn pdf(ops: Vec<Operation>, form: Option<Vec<Operation>>) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut resources = dictionary! {};
        if let Some(form_ops) = form {
            let body = Content {
                operations: form_ops,
            }
            .encode()
            .unwrap();
            let form = Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
                    "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 100.into(), 0.into()],
                },
                body,
            );
            let form_id = doc.add_object(form);
            resources.set("XObject", dictionary! { "F1" => form_id });
        }
        let content = Content { operations: ops };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 },
            ),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    pub(crate) fn op(name: &str, args: &[f64]) -> Operation {
        Operation::new(name, args.iter().map(|v| Object::Real(*v as f32)).collect())
    }

    #[test]
    fn points_scale_to_mm_and_holes_are_found() {
        // 72 x 72 pt square (25.4 mm) with a 36 pt square hole, filled even-odd.
        let ops = vec![
            op("re", &[0.0, 0.0, 72.0, 72.0]),
            op("re", &[18.0, 18.0, 36.0, 36.0]),
            op("f*", &[]),
        ];
        let r = import_ai(&pdf(ops, None)).unwrap();
        assert_eq!(r.parts.len(), 1);
        let g = &r.parts[0].geometry;
        assert!((g.bbox().width() - 25.4).abs() < 1e-3);
        assert_eq!(g.holes.len(), 1);
    }

    #[test]
    fn cm_and_forms_apply() {
        // A 2x scale cm, then a curve shape; and a form placed 100 pt to the right.
        let ops = vec![
            op("q", &[]),
            op("cm", &[2.0, 0.0, 0.0, 2.0, 0.0, 0.0]),
            op("m", &[0.0, 0.0]),
            op("l", &[36.0, 0.0]),
            op("c", &[36.0, 20.0, 20.0, 36.0, 0.0, 36.0]),
            op("h", &[]),
            op("S", &[]),
            op("Q", &[]),
            Operation::new("Do", vec![Object::Name(b"F1".to_vec())]),
        ];
        let form = vec![op("re", &[0.0, 0.0, 36.0, 36.0]), op("f", &[])];
        let r = import_ai(&pdf(ops, Some(form))).unwrap();
        assert_eq!(r.parts.len(), 2, "{:?}", r.warnings);
        let widths: Vec<f64> = r
            .parts
            .iter()
            .map(|p| (p.geometry.bbox().width() * 100.0).round() / 100.0)
            .collect();
        assert!(widths.contains(&25.4), "{widths:?}");
        assert!(widths.contains(&12.7), "{widths:?}");
    }

    #[test]
    fn clip_paths_are_ignored_and_text_warns() {
        let ops = vec![
            op("re", &[0.0, 0.0, 500.0, 500.0]),
            Operation::new("W", vec![]),
            op("n", &[]),
            op("re", &[10.0, 10.0, 20.0, 20.0]),
            op("f", &[]),
            Operation::new("BT", vec![]),
            Operation::new("Tj", vec![Object::string_literal("Hi")]),
            Operation::new("ET", vec![]),
        ];
        let r = import_ai(&pdf(ops, None)).unwrap();
        assert_eq!(r.parts.len(), 1);
        assert!(
            r.warnings.iter().any(|w| w.starts_with("Text in")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn unknown_data_is_rejected() {
        let err = import_ai(b"hello").unwrap_err();
        assert!(err.to_string().contains("isn't an Illustrator"), "{err}");
    }
}
