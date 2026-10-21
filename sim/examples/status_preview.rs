//! Renders the Identify-Printer status screen to target/status.png for a quick look.
//! Uses the same `render::status_lines` as the firmware, with sample data.
fn main() {
    use render::{LastJob, Line, PrinterStatus, Status};
    let status = Status {
        version: "v0.1.0-3-gabc1234",
        printer: Some(PrinterStatus {
            instance: "Inkplate 6 (a1b2)",
            hostname: "inkplate-a1b2",
            ip: Some([192, 168, 1, 42].into()),
            last_job: Some(LastJob { name: "Pancakes - Allrecipes", pages: 3, age: 150 }),
        }),
    };
    let lines = render::status_lines(&status);
    let lines: Vec<Line<'_>> = lines.iter().map(|(text, scale)| Line { text, scale: *scale }).collect();
    let img = render::text_screen(600, 800, &lines);
    image::GrayImage::from_raw(img.width, img.height, img.pixels).unwrap().save("target/status.png").unwrap();
}
