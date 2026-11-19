# Padgent eframe backport

This directory contains the published `eframe` 0.37.0 crate from crates.io,
licensed MIT OR Apache-2.0 (see its Cargo.toml and upstream repository).

The sole source change backports [egui PR #8832](https://github.com/emilk/egui/pull/8832):
`new_events` installs the active event-loop context while checking redraw
requests. Those checks can run UI for hidden windows, where creating an
immediate viewport otherwise fails. Remove this patch when upgrading to an
upstream eframe release containing that fix.
