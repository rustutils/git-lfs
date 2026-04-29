// git-lfs logo
// Compile: typst compile logo.typ logo.svg
//      or: typst compile logo.typ logo.png --ppi 300

#import "@preview/cetz:0.5.0"

// ---- Brand palette ----------------------------------------------------------
// Orange shared with the procutils sibling project; we drop the green/teal
// metric colors that fit /proc but not LFS, and pick a darker-orange shadow
// tone to give the mark the same two-tone isometric feel upstream uses
// without copying upstream's reds.
#let bg          = rgb("#1a1a1a")
#let orange      = rgb("#d97757")
#let orange-dark = rgb("#a85a3e")

// ---- Page: square, no margins, dark background ----------------------------
#set page(width: 680pt, height: 680pt, margin: 0pt, fill: bg)

// ---- Mark geometry (transcribed from upstream's SVG viewBox 49.02 x 48.07)
// Two filled polygons make an isometric chevron / pointer. We keep the shape
// so the projection family-resemblance to upstream stays legible; the only
// thing that changes is the palette.
#let mark-bright = (
  (39.56,  8.73), (15.13, 22.84), (15.13, 30.77), ( 9.46, 27.50),
  ( 9.46, 19.57), (33.90,  5.46), (24.44,  0.00), ( 0.00, 14.11),
  ( 0.00, 33.88), (24.59, 48.07), (24.59, 28.31), (49.02, 14.20),
)
#let mark-shadow = (
  (24.51, 28.18), (24.59, 48.07), (49.02, 33.96), (49.02, 14.20),
)
#let mark-vb-h = 48.07

#cetz.canvas(length: 1pt, {
  import cetz.draw: *

  // Anchor canvas bbox to the page so coords are page-relative.
  hide(rect((0, 0), (680, 680)), bounds: true)

  // Scale upstream's tiny viewBox up to ~480pt and center on the canvas.
  // SVG y is top-down; cetz y is bottom-up, so we flip y on the way in.
  let scale = 10.0
  let cx = 340 - (49.02 * scale) / 2
  let cy = 340 - (mark-vb-h * scale) / 2
  let place = (p) => (cx + p.at(0) * scale, cy + (mark-vb-h - p.at(1)) * scale)

  let bright = mark-bright.map(place)
  let shadow = mark-shadow.map(place)

  line(..bright, close: true, stroke: none, fill: orange)
  line(..shadow, close: true, stroke: none, fill: orange-dark)
})
