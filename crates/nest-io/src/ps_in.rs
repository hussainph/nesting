//! Illustrator files in the older PostScript format: .ai saved as Illustrator 8 or earlier,
//! .ai exported by CorelDRAW and other programs, and Illustrator .eps. The artwork after the
//! setup section is a list of simple path operators (`m`, `l`, `c`, `f`, `S`, ...) in points.

use crate::RawImport;
use anyhow::Result;
use kurbo::{BezPath, Point};

const MM_PER_PT: f64 = 25.4 / 72.0;

/// True for PostScript data, with or without the binary header some EPS files carry.
pub(crate) fn is_postscript(data: &[u8]) -> bool {
    postscript(data).starts_with(b"%!")
}

/// The PostScript section, skipping the binary header (and preview) of DOS-style EPS files.
fn postscript(data: &[u8]) -> &[u8] {
    if data.len() >= 12 && data[..4] == [0xC5, 0xD0, 0xD3, 0xC6] {
        let start = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
        if start < data.len() {
            return &data[start..start.saturating_add(len).min(data.len())];
        }
    }
    data
}

pub(crate) fn read(data: &[u8]) -> Result<RawImport> {
    let text = String::from_utf8_lossy(postscript(data));
    // The prolog and setup define procedures; the artwork starts after them.
    let start = ["%%EndSetup", "%%EndProlog"]
        .iter()
        .find_map(|m| text.find(m).map(|i| i + m.len()))
        .unwrap_or(0);
    let mut interp = Interp::default();
    // Embedded files, images and resources sit between Begin/End comments.
    let mut skip = 0usize;
    for line in text[start..].split(['\r', '\n']) {
        let line = line.trim();
        if let Some(comment) = line.strip_prefix("%%") {
            if ["BeginDocument", "BeginData", "BeginBinary", "BeginResource"].iter().any(|b| comment.starts_with(b)) {
                skip += 1;
            } else if ["EndDocument", "EndData", "EndBinary", "EndResource"].iter().any(|e| comment.starts_with(e)) {
                skip = skip.saturating_sub(1);
            } else if skip == 0 && (comment.starts_with("Trailer") || comment.starts_with("EOF")) {
                break;
            }
            continue;
        }
        if skip > 0 || line.starts_with('%') {
            continue;
        }
        interp.line(line);
    }
    let mut warnings = Vec::new();
    if interp.saw_text {
        warnings.push("Text in {file} wasn't imported. Convert text to outlines first.".to_string());
    }
    Ok(RawImport { groups: vec![interp.out], warnings })
}

#[derive(Default)]
struct Interp {
    /// Subpaths of the path under construction, in mm, with whether each is closed.
    path: Vec<(BezPath, bool)>,
    current: Option<Point>,
    in_text: bool,
    saw_text: bool,
    out: Vec<BezPath>,
}

impl Interp {
    fn line(&mut self, line: &str) {
        let mut nums: Vec<f64> = Vec::new();
        // Set by strings, arrays, names and procedures: the next operator isn't a path operator.
        let mut other = false;
        for token in line.split_whitespace() {
            if let Ok(v) = token.parse::<f64>() {
                nums.push(v);
                continue;
            }
            if token.starts_with(['(', '[', '/', '{', '<', ')', ']', '}', '>']) {
                other = true;
                continue;
            }
            if !other {
                self.op(token, &nums);
            } else if matches!(token, "Tx" | "TX" | "Tj" | "Tk") {
                self.saw_text = true;
            }
            nums.clear();
            other = false;
        }
    }

    fn op(&mut self, op: &str, n: &[f64]) {
        match op {
            "To" | "*To" => self.in_text = true,
            "TO" | "*TO" => self.in_text = false,
            "Tx" | "TX" | "Tj" | "Tk" => self.saw_text = true,
            _ if self.in_text => {}
            "m" if n.len() >= 2 => {
                let p = pt(&n[n.len() - 2..]);
                let mut sub = BezPath::new();
                sub.move_to(p);
                self.path.push((sub, false));
                self.current = Some(p);
            }
            "l" | "L" if n.len() >= 2 => {
                let p = pt(&n[n.len() - 2..]);
                self.started().line_to(p);
                self.current = Some(p);
            }
            "c" | "C" if n.len() >= 6 => {
                let n = &n[n.len() - 6..];
                let (a, b, p) = (pt(&n[0..2]), pt(&n[2..4]), pt(&n[4..6]));
                self.started().curve_to(a, b, p);
                self.current = Some(p);
            }
            "v" | "V" if n.len() >= 4 => {
                let n = &n[n.len() - 4..];
                let a = self.current.unwrap_or(Point::ZERO);
                let (b, p) = (pt(&n[0..2]), pt(&n[2..4]));
                self.started().curve_to(a, b, p);
                self.current = Some(p);
            }
            "y" | "Y" if n.len() >= 4 => {
                let n = &n[n.len() - 4..];
                let (a, p) = (pt(&n[0..2]), pt(&n[2..4]));
                self.started().curve_to(a, p, p);
                self.current = Some(p);
            }
            // Lowercase paint operators close the path first. Fills close everything;
            // strokes keep open paths open.
            "f" | "F" | "b" | "B" => self.commit(true),
            "s" => {
                self.close();
                self.commit(false);
            }
            "S" => self.commit(false),
            // Unpainted paths are clipping masks and guides.
            "n" | "N" => {
                self.path.clear();
                self.current = None;
            }
            _ => {}
        }
    }

    /// The open subpath to draw into, starting one at the current point if needed.
    fn started(&mut self) -> &mut BezPath {
        if self.path.last().is_none_or(|(_, closed)| *closed) {
            let mut sub = BezPath::new();
            sub.move_to(self.current.unwrap_or(Point::ZERO));
            self.path.push((sub, false));
        }
        &mut self.path.last_mut().expect("pushed above").0
    }

    fn close(&mut self) {
        if let Some((sub, closed)) = self.path.last_mut()
            && !*closed
        {
            sub.close_path();
            *closed = true;
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
}

fn pt(n: &[f64]) -> Point {
    Point::new(n[0] * MM_PER_PT, n[1] * MM_PER_PT)
}

#[cfg(test)]
mod tests {
    use crate::import_ai;

    /// An Illustrator 8 style file: a 72 pt square with a 36 pt square hole as a compound
    /// path, a curved shape, a clipping mask and a text object.
    const AI8: &str = "%!PS-Adobe-3.0\r%%Creator: Adobe Illustrator(R) 8.0\r%%BoundingBox: 0 0 300 100\r\
%%BeginProlog\r/m {moveto} def\r0 0 m\r%%EndProlog\r%%BeginSetup\r%%EndSetup\r\
%AI5_BeginLayer\r0 A\r*u\r0 O\r0 g\r0 0 m\r72 0 L\r72 72 L\r0 72 L\r0 0 L\rf\r\
18 18 m\r18 54 L\r54 54 L\r54 18 L\r18 18 L\rf\r*U\r\
100 0 m\r172 0 l\r172 40 172 72 136 72 c\r100 72 100 40 v\rb\r\
0 0 m 300 0 L 300 100 L 0 100 L W n\r\
0 To\r1 0 0 1 200 10 0 Tp\rTP\r(Hi) Tx\rTO\r\
%%PageTrailer\r%%Trailer\r200 0 m\r250 0 L\r250 50 L\rf\r%%EOF\r";

    #[test]
    fn reads_illustrator_8_files() {
        let r = import_ai(AI8.as_bytes()).unwrap();
        assert_eq!(r.parts.len(), 2, "{:?}", r.warnings);
        let square = r.parts.iter().find(|p| p.geometry.holes.len() == 1).expect("square with hole");
        assert!((square.geometry.bbox().width() - 25.4).abs() < 1e-3);
        assert!(r.warnings.iter().any(|w| w.starts_with("Text in")), "{:?}", r.warnings);
    }

    #[test]
    fn reads_eps_with_binary_header() {
        let ps = AI8.as_bytes();
        let mut data = vec![0xC5, 0xD0, 0xD3, 0xC6];
        data.extend_from_slice(&30u32.to_le_bytes());
        data.extend_from_slice(&(ps.len() as u32).to_le_bytes());
        data.resize(30, 0);
        data.extend_from_slice(ps);
        assert_eq!(import_ai(&data).unwrap().parts.len(), 2);
    }
}
