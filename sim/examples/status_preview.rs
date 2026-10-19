//! Renders the Identify-Printer status screen to target/status.png for a quick look.
fn main() {
    use render::Line;
    let lines = [
        Line { text: "Inkprint", scale: 3 },
        Line { text: "Inkplate 6 (a1b2)", scale: 2 },
        Line { text: "", scale: 1 },
        Line { text: "Ready to print", scale: 2 },
        Line { text: "inkplate-a1b2.local", scale: 2 },
        Line { text: "IP 192.168.1.42", scale: 2 },
        Line { text: "", scale: 1 },
        Line { text: "Last job: Pancakes - Allrecipes", scale: 2 },
        Line { text: "3 pages, 2 min ago", scale: 2 },
    ];
    let img = render::text_screen(600, 800, &lines);
    image::GrayImage::from_raw(img.width, img.height, img.pixels).unwrap().save("target/status.png").unwrap();
}
