//! Pure mouse-selection state for read-only text views.
//!
//! Endpoints are `(line, byte)` pairs into a view's *display* strings. All
//! byte offsets are kept on grapheme boundaries by [`column_to_byte`], so the
//! drawing helpers can slice without panicking on a split multi-byte char.

/// A position in a view's display text: a line index and a byte offset into
/// that line's display string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPos {
    pub line: usize,
    pub byte: usize,
}

/// The two ends of a mouse drag. `anchor` is where the button went down and
/// `head` follows the pointer; [`Self::ordered`] puts them in visual order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: Option<TextPos>,
    pub head: Option<TextPos>,
}

impl TextSelection {
    pub fn is_empty(&self) -> bool {
        self.anchor.is_none() || self.head.is_none() || self.anchor == self.head
    }

    /// Start a fresh drag at `pos` (anchor and head coincide until extended).
    pub fn begin(&mut self, pos: TextPos) {
        self.anchor = Some(pos);
        self.head = Some(pos);
    }

    /// Move the drag head. An `extend` without a `begin` is ignored so a
    /// stray `MouseMove` cannot create a selection out of nothing.
    pub fn extend(&mut self, pos: TextPos) {
        if self.anchor.is_some() {
            self.head = Some(pos);
        }
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
    }

    /// `(start, end)` in visual order (top-to-bottom, then left-to-right).
    pub fn ordered(&self) -> Option<(TextPos, TextPos)> {
        let a = self.anchor?;
        let b = self.head?;
        Some(if (a.line, a.byte) <= (b.line, b.byte) {
            (a, b)
        } else {
            (b, a)
        })
    }

    /// Copy the selected characters, inserting `\n` between lines. The start
    /// line contributes from its byte to the end; interior lines in full; the
    /// end line from byte 0 to its byte.
    pub fn selected_text<'a>(&self, lines: impl Iterator<Item = &'a str>) -> String {
        let Some((start, end)) = self.ordered() else {
            return String::new();
        };
        let mut out = String::new();
        for (i, line) in lines.enumerate() {
            if i < start.line {
                continue;
            }
            if i > end.line {
                break;
            }
            let from = if i == start.line {
                clamp_byte(line, start.byte)
            } else {
                0
            };
            let to = if i == end.line {
                clamp_byte(line, end.byte)
            } else {
                line.len()
            };
            if from < to {
                out.push_str(&line[from..to]);
            }
            if i < end.line {
                out.push('\n');
            }
        }
        out
    }

    /// Byte ranges of `display` that fall inside the selection, each tagged
    /// `selected`. Endpoints split a line into at most three chunks so a
    /// viewer can draw selected and unselected spans with different styles.
    pub fn segments_for_line(&self, line: usize, display: &str) -> Vec<(usize, usize, bool)> {
        let len = display.len();
        if len == 0 {
            return Vec::new();
        }
        let Some((start, end)) = self.ordered() else {
            return vec![(0, len, false)];
        };
        if line < start.line || line > end.line {
            return vec![(0, len, false)];
        }
        // Same endpoints as `selected_text`: interior lines contribute in
        // full. `end.byte` is an offset into the END line only — using it on
        // a middle line would highlight a prefix while Copy takes it all.
        let from = if line == start.line {
            clamp_byte(display, start.byte)
        } else {
            0
        };
        let to = if line == end.line {
            clamp_byte(display, end.byte)
        } else {
            len
        };
        let from = from.min(to);
        let mut cuts = vec![0, len];
        if from > 0 && from < len {
            cuts.push(from);
        }
        if to > 0 && to < len {
            cuts.push(to);
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut segs = Vec::with_capacity(cuts.len().saturating_sub(1));
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a < b {
                segs.push((a, b, a >= from && b <= to));
            }
        }
        segs
    }
}

/// Step `byte` back to a valid `char` boundary of `display` (clamping past
/// the end first). Keeps slicing safe even for stale/unaligned endpoints.
fn clamp_byte(display: &str, byte: usize) -> usize {
    let mut b = byte.min(display.len());
    while b > 0 && !display.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Convert a display **column** to a byte offset into `display`. A column
/// inside a double-width grapheme is snapped to just *after* that grapheme
/// (`scroll(_, _, true)`), never into the middle of its UTF-8 bytes.
pub fn column_to_byte(display: &str, column: i32) -> usize {
    if column <= 0 {
        return 0;
    }
    tv::text::scroll(display, column, true).0
}

/// Display column of a byte offset (clamped to a valid boundary first).
pub fn byte_to_column(display: &str, byte: usize) -> i32 {
    tv::text::width(&display[..clamp_byte(display, byte)]) as i32
}

/// Map an absolute pointer position to a `(line, byte)` position in a view
/// whose top-left cell is at `origin`, scrolled down by `scroll` lines.
/// `byte` is a column here; callers snap it with [`column_to_byte`] using the
/// hit line's display string before storing it in a [`TextSelection`].
pub fn hit_test(origin: tv::Point, abs: tv::Point, scroll: i32, lines: usize) -> Option<TextPos> {
    let local = tv::Point::new(abs.x - origin.x, abs.y - origin.y);
    let line = local.y + scroll;
    if line < 0 || line as usize >= lines {
        return None;
    }
    Some(TextPos {
        line: line as usize,
        byte: local.x.max(0) as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tv::Point;

    #[test]
    fn column_to_byte_snaps_past_a_wide_grapheme() {
        // 'a' occupies col 0; '界' spans cols 1..3; 'b' is at col 3.
        let display = "a界b";
        assert_eq!(column_to_byte(display, 0), 0);
        assert_eq!(column_to_byte(display, 1), 1, "after 'a'");
        assert_eq!(
            column_to_byte(display, 2),
            4,
            "a click in 界's trailing cell lands AFTER the whole grapheme"
        );
        assert_eq!(column_to_byte(display, 3), 4);
        assert_eq!(
            column_to_byte(display, -5),
            0,
            "negative columns clamp to 0"
        );
        assert_eq!(byte_to_column(display, 4), 3, "byte 4 is display column 3");
        assert_eq!(byte_to_column(display, column_to_byte(display, 2)), 3);
    }

    #[test]
    fn byte_to_column_round_trips_ascii_and_clamps_unaligned_bytes() {
        assert_eq!(byte_to_column("hello", 0), 0);
        assert_eq!(byte_to_column("hello", 3), 3);
        assert_eq!(byte_to_column("hello", 99), 5, "past the end clamps");
        // byte 2 is inside 界 (bytes 1..4); it must step back to byte 1.
        assert_eq!(byte_to_column("a界b", 2), 1);
    }

    #[test]
    fn reverse_drag_joins_lines_in_visual_order() {
        // begin on the LAST line and drag backwards to the first.
        let lines = ["prefix mid", "all", "head suffix"];
        let mut sel = TextSelection::default();
        sel.begin(TextPos { line: 2, byte: 4 }); // "head"
        sel.extend(TextPos { line: 0, byte: 7 }); // "mid"
        assert_eq!(sel.selected_text(lines.iter().copied()), "mid\nall\nhead");
        assert_eq!(
            sel.ordered(),
            Some((TextPos { line: 0, byte: 7 }, TextPos { line: 2, byte: 4 }))
        );
    }

    #[test]
    fn a_selection_ending_at_byte_zero_of_the_next_line_has_no_text_on_it() {
        let lines = ["abc", "def"];
        let mut sel = TextSelection::default();
        sel.begin(TextPos { line: 0, byte: 1 });
        sel.extend(TextPos { line: 1, byte: 0 });
        assert_eq!(sel.selected_text(lines.iter().copied()), "bc\n");
        assert_eq!(
            sel.segments_for_line(1, "def"),
            vec![(0, 3, false)],
            "the second line is entirely unselected"
        );
    }

    #[test]
    fn unaligned_endpoints_are_clamped_without_panicking() {
        let display = "a界b"; // 界 owns bytes 1..4; byte 2 is mid-grapheme.
        let mut sel = TextSelection::default();
        sel.begin(TextPos { line: 0, byte: 2 });
        sel.extend(TextPos { line: 0, byte: 3 });
        let segs = sel.segments_for_line(0, display);
        assert!(
            segs.iter()
                .all(|&(a, b, _)| display.is_char_boundary(a) && display.is_char_boundary(b)),
            "every emitted range must be sliceable: {segs:?}"
        );
        let _ = sel.selected_text(std::iter::once(display));
    }

    #[test]
    fn segments_split_at_both_endpoints() {
        let mut sel = TextSelection::default();
        sel.begin(TextPos { line: 0, byte: 2 });
        sel.extend(TextPos { line: 0, byte: 5 });
        assert_eq!(
            sel.segments_for_line(0, "0123456789"),
            vec![(0, 2, false), (2, 5, true), (5, 10, false)]
        );
        assert_eq!(
            sel.segments_for_line(1, "other"),
            vec![(0, 5, false)],
            "unrelated lines stay fully unselected"
        );
    }

    #[test]
    fn interior_lines_are_highlighted_in_full_even_when_the_end_line_is_short() {
        let lines = ["abcdefghij", "0123456789", "XYZ"];
        let mut sel = TextSelection::default();
        sel.begin(TextPos { line: 0, byte: 2 });
        sel.extend(TextPos { line: 2, byte: 1 });
        assert_eq!(
            sel.selected_text(lines.iter().copied()),
            "cdefghij\n0123456789\nX"
        );
        // The middle line contributes to Copy in full, so its highlight
        // must cover all ten bytes — not `end.byte` (1) bytes of it.
        assert_eq!(
            sel.segments_for_line(1, lines[1]),
            vec![(0, 10, true)],
            "highlight matches the copied text"
        );
        assert_eq!(
            sel.segments_for_line(2, lines[2]),
            vec![(0, 1, true), (1, 3, false)]
        );
    }

    #[test]
    fn hit_test_maps_absolute_coordinates_to_a_line_and_byte() {
        let origin = Point::new(5, 2);
        let last = 5;
        assert_eq!(
            hit_test(origin, Point::new(7, 2), 0, last),
            Some(TextPos { line: 0, byte: 2 })
        );
        assert_eq!(
            hit_test(origin, Point::new(5, 4), 0, last),
            Some(TextPos { line: 2, byte: 0 })
        );
        assert_eq!(
            hit_test(origin, Point::new(3, 3), 0, last),
            Some(TextPos { line: 1, byte: 0 }),
            "a click left of the text origin clamps to byte 0"
        );
    }

    #[test]
    fn hit_test_accounts_for_scroll_and_rejects_out_of_range_rows() {
        let origin = Point::new(0, 0);
        assert_eq!(
            hit_test(origin, Point::new(3, 2), 10, 20),
            Some(TextPos { line: 12, byte: 3 }),
            "a scrolled-down view maps screen row 2 to line 12"
        );
        assert_eq!(
            hit_test(origin, Point::new(0, 5), 0, 5),
            None,
            "a click below the last line selects nothing"
        );
        assert_eq!(
            hit_test(origin, Point::new(0, 9), 10, 15),
            None,
            "the same, with a scroll offset"
        );
        assert_eq!(
            hit_test(origin, Point::new(0, -1), 0, 5),
            None,
            "above the first line selects nothing"
        );
    }

    #[test]
    fn selection_lifecycle_is_begin_extend_clear() {
        let mut sel = TextSelection::default();
        assert!(sel.is_empty());
        sel.extend(TextPos { line: 0, byte: 3 });
        assert!(sel.is_empty(), "extend without a begin is ignored");
        sel.begin(TextPos { line: 1, byte: 1 });
        assert!(
            sel.is_empty(),
            "anchor and head coincide: no characters selected yet"
        );
        sel.extend(TextPos { line: 1, byte: 4 });
        assert!(!sel.is_empty(), "dragging to a new byte selects characters");
        assert_eq!(
            sel.ordered(),
            Some((TextPos { line: 1, byte: 1 }, TextPos { line: 1, byte: 4 }))
        );
        sel.clear();
        assert!(sel.is_empty());
        assert_eq!(sel.ordered(), None);
    }
}
