// git-lfs banner — light/dark variants on a transparent background.
// Compile: typst compile banner.typ banner-dark.svg  --input theme=dark
//      or: typst compile banner.typ banner-light.svg --input theme=light
// (default theme is dark)

#import "@preview/cetz:0.5.0"

// ---- Theme switch -----------------------------------------------------------
#let theme = sys.inputs.at("theme", default: "dark")

// ---- Brand palette ----------------------------------------------------------
// Orange + dark-orange shadow shared with the logo; fg/muted track the
// procutils sibling banner so the family looks consistent.
#let orange      = rgb("#d97757")
#let orange-dark = rgb("#a85a3e")
#let fg      = if theme == "light" { rgb("#1a1a1a") } else { rgb("#f5f1eb") }
#let muted   = if theme == "light" { rgb("#6a6055") } else { rgb("#a89888") }
#let divider = orange.transparentize(60%)

// ---- Page: 680x240 banner, no margins, transparent background -------------
#set page(width: 680pt, height: 240pt, margin: 0pt, fill: none)
#set text(font: "Inter", fill: fg)

// Mark geometry — same transcription as logo.typ.
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

  hide(rect((0, 0), (680, 240)), bounds: true)

  // Mark on the left. Roughly 130pt tall, centered vertically around y=120.
  let scale = 2.7
  let mx = 50
  let my = 120 - (mark-vb-h * scale) / 2
  let place = (p) => (mx + p.at(0) * scale, my + (mark-vb-h - p.at(1)) * scale)

  let bright = mark-bright.map(place)
  let shadow = mark-shadow.map(place)
  line(..bright, close: true, stroke: none, fill: orange)
  line(..shadow, close: true, stroke: none, fill: orange-dark)

  // Vertical divider between mark and wordmark, matching procutils' banner.
  line((210, 70), (210, 170), stroke: (paint: divider, thickness: 1pt))

  // Wordmark + tagline. "git-**lfs**" mirrors procutils' "proc**utils**"
  // accent — last segment in orange.
  content(
    (240, 135),
    anchor: "west",
    text(size: 56pt, weight: 500, tracking: -1pt)[
      git-#text(fill: orange)[lfs]
    ],
  )

  content(
    (240, 80),
    anchor: "west",
    text(font: "CommitMono", size: 14pt, fill: muted, tracking: 0.5pt)[
      git-lfs, implemented in rust
    ],
  )
})
