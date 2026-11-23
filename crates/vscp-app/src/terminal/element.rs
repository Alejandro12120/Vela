// Adapted from Zed crates/terminal_view/src/terminal_element.rs (v1.27.0), GPL-3.0-or-later.

use alacritty_terminal::{
    index::{Column, Direction, Line, Point as CellPoint},
    term::cell::Flags,
    vte::ansi::{CursorShape, NamedColor},
};
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, StrikethroughStyle, Style, TextAlign,
    TextRun, UnderlineStyle, Window, fill, point, px, relative, rgb, size,
};

use super::{GridSize, view::TerminalView};

#[derive(Clone, Copy)]
pub(super) struct Geometry {
    pub origin: Point<Pixels>,
    pub height: Pixels,
    width: Pixels,
    scale: f32,
    size: GridSize,
}

impl Geometry {
    fn new(bounds: Bounds<Pixels>, view: &TerminalView, window: &Window) -> Self {
        let scale = window.scale_factor();
        let snap = |value: Pixels| (value * scale).round() / scale;
        let (width, height) = view.geometry.filter(|old| old.scale == scale).map_or_else(
            || {
                let text = window.text_system();
                let font = text.resolve_font(&view.fonts[0]);
                let font_size = px(view.settings.size);
                let advance = text
                    .advance(font, font_size, 'M')
                    .map_or(font_size * 0.6, |size| size.width);
                // Kitty rounds the monospaced advance up in device pixels.
                let width = (advance * scale).ceil() / scale;
                let line_height = native_line_height(&view.settings.family, font_size * scale)
                    .map_or_else(
                        || text.ascent(font, font_size) + text.descent(font, font_size),
                        |height| height / scale,
                    );
                let height = (line_height.max(font_size) * scale).ceil() / scale;
                (width.max(px(1.)), height.max(px(1.)))
            },
            |old| (old.width, old.height),
        );
        let padding = px(6.);
        Self {
            origin: point(
                snap(bounds.origin.x + padding),
                snap(bounds.origin.y + padding),
            ),
            width,
            height,
            scale,
            size: GridSize {
                cols: ((bounds.size.width - padding * 2.) / width)
                    .floor()
                    .clamp(2., 1000.) as u16,
                rows: ((bounds.size.height - padding * 2.) / height)
                    .floor()
                    .clamp(1., 1000.) as u16,
                cell_width: f32::from(width * scale).round() as u16,
                cell_height: f32::from(height * scale).round() as u16,
            },
        }
    }

    pub fn cell(&self, position: Point<Pixels>, offset: usize) -> (CellPoint, Direction) {
        let x = ((position.x - self.origin.x) / self.width).clamp(0., f32::from(self.size.cols));
        let column = (x as usize).min(usize::from(self.size.cols - 1));
        let row = ((position.y - self.origin.y) / self.height)
            .clamp(0., f32::from(self.size.rows - 1)) as i32;
        let side = if x - column as f32 >= 0.5 {
            Direction::Right
        } else {
            Direction::Left
        };
        (
            CellPoint::new(Line(row - offset as i32), Column(column)),
            side,
        )
    }

    fn rect(self, col: usize, row: i32, columns: usize) -> Bounds<Pixels> {
        Bounds::new(
            self.origin + point(self.width * col as f32, self.height * row as f32),
            size(self.width * columns as f32, self.height),
        )
    }
}

pub(super) struct Surface(pub Entity<TerminalView>);

impl IntoElement for Surface {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

#[derive(PartialEq, Eq)]
struct StyleKey {
    fg: u32,
    variant: usize,
    decoration: Flags,
    underline: Option<u32>,
}

struct Run {
    key: StyleKey,
    text: String,
    col: usize,
    row: i32,
    columns: usize,
    fixed: bool,
}

#[derive(Default)]
pub(super) struct Frame {
    backgrounds: Vec<(u32, Bounds<Pixels>)>,
    runs: Vec<Run>,
    cursor: Option<(CursorShape, u32, Bounds<Pixels>)>,
}

impl Element for Surface {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: size(relative(1.).into(), relative(1.).into()),
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Frame {
        self.0.update(cx, |view, cx| {
            let geometry = Geometry::new(bounds, view, window);
            view.geometry = Some(geometry);
            view.spawn_measured(geometry.size, window, cx);
            view.ime_bounds = None;
            let mut frame = Frame::default();
            let palette = &view.palette;
            let Some(terminal) = view.terminal.as_mut() else {
                frame.backgrounds.push((palette.background, bounds));
                return frame;
            };
            terminal.resize(geometry.size);
            let grid = terminal.grid.lock();
            let content = grid.renderable_content();
            let background = palette.color(
                alacritty_terminal::vte::ansi::Color::Named(NamedColor::Background),
                content.colors,
            );
            frame.backgrounds.push((background, bounds));
            let cursor = content.cursor.point;
            let focus = view.focus.is_focused(window) && window.is_window_active();
            let shape = if content.cursor.shape == CursorShape::Hidden || terminal.ended() {
                CursorShape::Hidden
            } else if focus {
                content.cursor.shape
            } else {
                CursorShape::HollowBlock
            };
            let mut background_run: Option<(u32, usize, i32, usize)> = None;

            for cell in content.display_iter {
                let row = cell.point.line.0 + content.display_offset as i32;
                let col = cell.point.column.0;
                let flags = cell.cell.flags;
                if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let columns = if flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };
                let mut fg = palette.color(cell.cell.fg, content.colors);
                let mut bg = palette.color(cell.cell.bg, content.colors);
                if flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if flags.contains(Flags::DIM) {
                    fg = dim(fg, bg);
                }
                if content.selection.is_some_and(|selection| {
                    selection.contains(cell.point)
                        || (columns == 2
                            && selection.contains(CellPoint::new(cell.point.line, Column(col + 1))))
                }) {
                    (fg, bg) = (
                        palette.selection_foreground.unwrap_or(
                            if palette.selection_background.is_none() {
                                bg
                            } else {
                                fg
                            },
                        ),
                        palette.selection_background.unwrap_or(fg),
                    );
                }
                if cell.point == cursor {
                    let rect = geometry.rect(col, row, columns);
                    view.ime_bounds = Some(rect);
                    let cursor_color = content.colors[NamedColor::Cursor]
                        .map(|rgb| u32::from_be_bytes([0, rgb.r, rgb.g, rgb.b]))
                        .or(palette.cursor)
                        .unwrap_or(fg);
                    if shape == CursorShape::Block {
                        fg = palette
                            .cursor_text
                            .filter(|_| palette.cursor.is_some())
                            .unwrap_or(bg);
                        bg = cursor_color;
                    } else {
                        frame.cursor = Some((shape, cursor_color, rect));
                    }
                }
                match &mut background_run {
                    Some((color, start, line, count))
                        if *color == bg && *line == row && *start + *count == col =>
                    {
                        *count += columns
                    }
                    _ => {
                        if let Some((color, start, line, count)) =
                            background_run.replace((bg, col, row, columns))
                            && color != background
                        {
                            frame
                                .backgrounds
                                .push((color, geometry.rect(start, line, count)));
                        }
                    }
                }
                let decoration = flags & (Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
                if flags.contains(Flags::HIDDEN) {
                    continue;
                }
                let key = StyleKey {
                    fg,
                    variant: usize::from(flags.contains(Flags::BOLD))
                        | (usize::from(flags.contains(Flags::ITALIC)) << 1),
                    decoration,
                    underline: cell
                        .cell
                        .underline_color()
                        .map(|color| palette.color(color, content.colors)),
                };
                let combining = cell.cell.zerowidth().unwrap_or_default();
                let fixed = columns == 1 && combining.is_empty();
                let character = if cell.cell.c.is_control() {
                    ' '
                } else {
                    cell.cell.c
                };
                if let Some(run) = frame.runs.last_mut()
                    && fixed
                    && run.fixed
                    && run.key == key
                    && run.row == row
                    && run.col + run.columns == col
                {
                    run.text.push(character);
                    run.columns += 1;
                } else {
                    let mut text = String::from(character);
                    text.extend(combining);
                    frame.runs.push(Run {
                        key,
                        text,
                        col,
                        row,
                        columns,
                        fixed,
                    });
                }
            }
            if let Some((color, col, row, columns)) = background_run
                && color != background
            {
                frame
                    .backgrounds
                    .push((color, geometry.rect(col, row, columns)));
            }
            if crate::dev::logging() && view.last_logged_size != Some(geometry.size) {
                view.last_logged_size = Some(geometry.size);
                crate::dev::log(format!(
                    "Terminal grid: {}×{}",
                    geometry.size.cols, geometry.size.rows
                ));
            }
            frame
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = self.0.read(cx);
        let geometry = view.geometry.unwrap();
        let fonts = view.fonts.clone();
        let font_size = px(view.settings.size);
        let focus = view.focus.clone();
        let ime = view
            .ime_bounds
            .zip(view.marked.as_ref())
            .map(|(rect, marked)| (rect, marked.text.replace(['\n', '\r'], " "), view.palette));
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for &(color, rect) in &frame.backgrounds {
                window.paint_quad(fill(rect, rgb(color)));
            }
            for run in &frame.runs {
                if run.key.decoration.is_empty() && run.text.bytes().all(|byte| byte == b' ') {
                    continue;
                }
                let rect = geometry.rect(run.col, run.row, run.columns);
                let flags = run.key.decoration;
                let foreground = rgb(run.key.fg).into();
                let underline = rgb(run.key.underline.unwrap_or(run.key.fg)).into();
                let shaped = window.text_system().shape_line(
                    run.text.clone().into(),
                    font_size,
                    &[TextRun {
                        len: run.text.len(),
                        font: fonts[run.key.variant].clone(),
                        color: foreground,
                        strikethrough: flags.contains(Flags::STRIKEOUT).then_some(
                            StrikethroughStyle {
                                color: Some(foreground),
                                thickness: px(1.),
                            },
                        ),
                        ..Default::default()
                    }],
                    run.fixed.then_some(geometry.width),
                );
                let _ = shaped.paint(
                    rect.origin,
                    geometry.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                if flags.intersects(Flags::ALL_UNDERLINES) {
                    let base = point(rect.left(), rect.bottom() - px(2.));
                    if flags.intersects(Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE) {
                        let segment = if flags.contains(Flags::DOTTED_UNDERLINE) {
                            1
                        } else {
                            3
                        };
                        let step = segment + 2;
                        for x in (0..f32::from(rect.size.width) as usize).step_by(step) {
                            let x = px(x as f32);
                            let width = px(segment as f32).min(rect.size.width - x);
                            window.paint_quad(fill(
                                Bounds::new(base + point(x, px(0.)), size(width, px(1.))),
                                underline,
                            ));
                        }
                    } else {
                        let style = UnderlineStyle {
                            color: Some(underline),
                            thickness: px(1.),
                            wavy: flags.contains(Flags::UNDERCURL),
                        };
                        window.paint_underline(base, rect.size.width, &style);
                        if flags.contains(Flags::DOUBLE_UNDERLINE) {
                            window.paint_underline(
                                base - point(px(0.), px(2.)),
                                rect.size.width,
                                &style,
                            );
                        }
                    }
                }
            }
            if let Some((shape, color, rect)) = frame.cursor {
                paint_cursor(shape, color, rect, window);
            }
            if let Some((rect, text, palette)) = ime {
                let shaped = window.text_system().shape_line(
                    text.clone().into(),
                    font_size,
                    &[TextRun {
                        len: text.len(),
                        font: fonts[0].clone(),
                        color: rgb(palette.foreground).into(),
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    None,
                );
                window.paint_quad(fill(
                    Bounds::new(rect.origin, size(shaped.width, geometry.height)),
                    rgb(palette.background),
                ));
                let _ = shaped.paint(
                    rect.origin,
                    geometry.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        });
    }
}

fn paint_cursor(shape: CursorShape, color: u32, rect: Bounds<Pixels>, window: &mut Window) {
    let mut bar = |x, y, width, height| {
        window.paint_quad(fill(
            Bounds::new(point(x, y), size(width, height)),
            rgb(color),
        ));
    };
    match shape {
        CursorShape::Beam => bar(rect.left(), rect.top(), px(1.5), rect.size.height),
        CursorShape::Underline => bar(rect.left(), rect.bottom() - px(2.), rect.size.width, px(2.)),
        CursorShape::HollowBlock => {
            bar(rect.left(), rect.top(), rect.size.width, px(1.));
            bar(rect.left(), rect.bottom() - px(1.), rect.size.width, px(1.));
            bar(rect.left(), rect.top(), px(1.), rect.size.height);
            bar(rect.right() - px(1.), rect.top(), px(1.), rect.size.height);
        }
        _ => {}
    }
}

fn dim(foreground: u32, background: u32) -> u32 {
    let channel = |shift: u32| {
        let fg = (foreground >> shift) & 255_u32;
        let bg = (background >> shift) & 255_u32;
        (fg * 2 + bg * 3) / 5
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// Use AppKit's typographic metrics for the same face at the physical size.
/// GPUI's font-kit backend can report a shorter ascent and descent for patched
/// fonts, making the grid denser than Kitty's CoreText renderer.
#[cfg(target_os = "macos")]
fn native_line_height(family: &str, device_size: Pixels) -> Option<Pixels> {
    use objc2::{class, msg_send, runtime::AnyObject};
    use objc2_foundation::NSString;

    let family = NSString::from_str(family);
    let size = f64::from(f32::from(device_size));

    // SAFETY: The font is borrowed from AppKit for the duration of these calls.
    // The selector signatures use CGFloat and NSUInteger on 64-bit macOS.
    unsafe {
        let mut font: *mut AnyObject =
            msg_send![class!(NSFont), fontWithName: &*family, size: size];
        if font.is_null() {
            let manager: *mut AnyObject = msg_send![class!(NSFontManager), sharedFontManager];
            font = msg_send![manager, fontWithFamily: &*family, traits: 0usize, weight: 5isize, size: size];
        }
        if font.is_null() {
            return None;
        }
        let ascent: f64 = msg_send![font, ascender];
        let descent: f64 = msg_send![font, descender];
        let leading: f64 = msg_send![font, leading];
        Some(px((ascent - descent + leading.max(0.0)) as f32))
    }
}

#[cfg(not(target_os = "macos"))]
fn native_line_height(_: &str, _: Pixels) -> Option<Pixels> {
    None
}
