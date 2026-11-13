# Unified History concept

Date: 2026-11-14. Status: proposed layout for owner review, not an implemented native application.

The owner requested one History window instead of separate History/Image history windows, and selected a compact dark desktop utility direction. The interactive HTML concept demonstrates Gallery/List switching, shared filters/search, selection, a collapsible preview/details pane, generic file cards, separate local availability and upload state, and checkbox selection.

All nine records and miniature captured-content illustrations are sample data. Desktop file/link/edit/pin/retry buttons describe their intended application actions when clicked; they do not access files, upload, open a browser, or alter the clipboard. The app implementation will remain Rust/egui; this HTML is a throwaway design artifact.

## Design review

- Single-font hook finding: contextually intentional. One consistent UI typeface fits the owner-selected compact native desktop utility. Adding a display family would create unnecessary visual distinction among related controls. Captured terminal illustrations use a source-content monospace face. No rule suppression or ignore entry was added.
- Flat-hierarchy hook finding: addressed by differentiating the 20px History title, 15px selected-file heading, 12px main controls, and smaller metadata, and by increasing card/list text and status sizes. Remaining tightly spaced control sizes are intentional for a compact utility; a promotional-page type scale is not appropriate here. The automated hook continues to flag its body-style heuristic; visual inspection determines hierarchy is sufficient for this concept. No rule was suppressed.
- Preview review found selected controls with insufficient foreground/background distinction under the host theme; selected buttons now use the primary foreground on the primary background. Selected list rows use a restrained tint to keep status text readable.

## Checked in the HTML preview

The inline preview reported no console errors. An isolated preview self-check passed these behaviors: List changes presentation; view switching preserves the selected record; search finds a ZIP file; switching back to Gallery retains the filtered selection; Failed filtering shows the retryable image; and a missing local image still exposes its confirmed upload link.

The final rendered artifact starts in Gallery. Use List and Preview to compare layouts; use filters/search or choose the uploaded item with Local file missing to inspect the two independent states. These checks validate only the design artifact, not native application behavior.
