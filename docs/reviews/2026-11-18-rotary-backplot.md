# 4th axis in the backplot (2026-11-18)

## What millgui did before

- Every renderer drew X Y Z only. A program on the rotary showed each pass
  flat on top of the last; a move that only turns A drew nothing.
- The live tool, its trail and the tool cylinder ignored A.
- Time estimates counted a move that only turns A as zero seconds, and a
  wrap move (X with many turns of A) as the X move alone.
- The travel check is fine: `mymill.ini` gives A no limits (wrapped rotary).

## What it does now

- When a program turns A (or works at a fixed A other than 0), the
  toolpath is drawn wrapped around the rotary: about X, through work Y0 Z0
  (where the rotary setup probe puts them). Programs that leave A at 0 draw
  exactly as before.
- The sense comes from `[DISPLAY]GEOMETRY` the way LinuxCNC's AXIS reads it
  (`xyza` turns the drawing with A, `-A` the other way). With +A the tool
  goes from the top of the part toward -Y (RS274: A is the tool's motion
  relative to the part, right hand about +X).
- A move that turns A is cut into chords of at most 2 degrees.
- The live tool, trail and tool cylinder use the same wrap: the tool leans
  around the part with A. The rotary centerline is a dashed line.
- Time: XYZ length at the feed, or degrees at the feed for an A-only move
  (as LinuxCNC does, also for G93), and never faster than A's rapid rate
  (180 deg/s).
- FILES summary adds how far A turns.

## Found on the way: GPU backplot showed no live tool

On main (since the 2026-11-18 performance pass) the GPU backplot drew every
frame into the texture already shown, and Slint draws an image again only
when the image property changes. So the live tool, trail and tool cylinder
stayed where they were at program load. The backplot now draws into two
textures in turn and points the image at the next one before the frame (in
`redraw`), so each redraw is seen and costs no extra frame. Sim (llvmpipe,
GPU renderer): idle 0 frames/s, program running 21 to 26 frames/s.

## Checked

LinuxCNC 2.9.10 sim with the mill's INI (XYZA, wrapped rotary), both
renderers: `testdata/nc_files/rotary-wrap-sample.ngc` (helical rough, four
flats at A0/90/180/270, a spiral groove). The trail of the real motion lies
on the wrapped preview. Note for programs: a wrapped rotary refuses absolute
A beyond 360 (`Invalid absolute position 3600.00 for wrapped rotary axis
A`); multi-turn moves use G91.

## Left as is

- The A DRO shows LinuxCNC's unwrapped angle (e.g. 4740°).
- The trail (white) covers the op colours where the tool has been.
- The Mac stand-in reader (`mini`) does not know wrapped rotary rules.
