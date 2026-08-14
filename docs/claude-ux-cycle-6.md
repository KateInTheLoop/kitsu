# Claude UX cycle 6: responsive composer surfaces

## Challenge

Open Kitsu and Claude in a 50-column, 24-row PTY, type `/`, and compare the
composer footer and command-list geometry. Ask the configured local Ollama model
for a separate assessment of the fixed-width behavior.

## Observation

Kitsu emitted 70-cell palette and footer rows regardless of the 50-column PTY.
The footer wrapped onto two physical rows; opening `/` made Rustyline account
for 24 physical rows instead of the 12 logical hint rows. Claude sized its
frame and command list to exactly 50 columns.

The local model recognized the fixed-width mismatch and excessive redraw as the
primary defect. Its suggested CSS pixel breakpoints did not apply to a terminal
CLI, so the observed PTY geometry and Claude comparison drove the implementation.

## Improvement

The slash palette and model/directory footer now use this policy:

- outer width is `min(current terminal columns, 70)`;
- terminal width is checked on every hint redraw, allowing live pane resizing;
- below 42 columns, palette rows show command syntax without descriptions;
- truncation and padding use Unicode display-cell width, not scalar count;
- when terminal dimensions are unavailable, the established 70-column width is
  retained.

Rich Unicode and `NO_COLOR` ASCII variants share the same geometry.

## Verification

A pure layout test covers 32-, 50-, 70-, and 120-column inputs, including
wide-character model and directory names. A Unix PTY integration test launches
the real editor at 50 columns, opens `/`, and verifies every palette/footer
boundary row is exactly 50 display cells before exiting cleanly.
