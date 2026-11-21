use gpui::{IntoElement, PathBuilder, Rgba, Styled, canvas, point, px};

#[derive(Clone, Copy)]
pub enum Icon {
    Back,
    Forward,
    Up,
    Down,
    Refresh,
    Hidden,
    Sidebar,
    Folder,
    File,
    Link,
    Transfer,
    TransferBack,
}

impl Icon {
    pub fn draw(self, color: Rgba) -> impl IntoElement {
        let lines: &[&[(f32, f32)]] = match self {
            Self::Back => &[&[(9., 4.), (5., 8.), (9., 12.)]],
            Self::Forward => &[&[(6., 4.), (10., 8.), (6., 12.)]],
            Self::Up => &[&[(4., 7.), (8., 3.), (12., 7.)], &[(8., 3.), (8., 13.)]],
            Self::Down => &[&[(4., 6.), (8., 10.), (12., 6.)]],
            Self::Refresh => &[
                &[
                    (13., 6.),
                    (11., 3.5),
                    (7., 3.),
                    (4., 5.),
                    (3., 8.),
                    (4., 11.),
                    (7., 13.),
                    (11., 12.5),
                    (13., 10.),
                ],
                &[(13., 3.), (13., 6.5), (9.5, 6.5)],
            ],
            Self::Hidden => &[
                &[
                    (1.5, 8.),
                    (4.5, 5.),
                    (8., 4.),
                    (11.5, 5.),
                    (14.5, 8.),
                    (11.5, 11.),
                    (8., 12.),
                    (4.5, 11.),
                    (1.5, 8.),
                ],
                &[(8., 6.5), (9.5, 8.), (8., 9.5), (6.5, 8.), (8., 6.5)],
            ],
            Self::Sidebar => &[
                &[(2., 3.), (14., 3.), (14., 13.), (2., 13.), (2., 3.)],
                &[(6.5, 3.), (6.5, 13.)],
            ],
            Self::Folder => &[&[
                (1.5, 12.5),
                (1.5, 4.),
                (6.5, 4.),
                (8., 5.5),
                (14.5, 5.5),
                (14.5, 12.5),
                (1.5, 12.5),
            ]],
            Self::File => &[
                &[
                    (4., 2.),
                    (9., 2.),
                    (12.5, 5.5),
                    (12.5, 14.),
                    (4., 14.),
                    (4., 2.),
                ],
                &[(9., 2.), (9., 5.5), (12.5, 5.5)],
            ],
            Self::TransferBack => &[&[(14., 8.), (2., 8.)], &[(6., 4.), (2., 8.), (6., 12.)]],
            Self::Transfer => &[&[(2., 8.), (14., 8.)], &[(10., 4.), (14., 8.), (10., 12.)]],
            Self::Link => &[
                &[
                    (7., 11.),
                    (5., 13.),
                    (3., 13.),
                    (2., 11.),
                    (2.5, 9.),
                    (5., 6.5),
                    (7., 6.5),
                ],
                &[
                    (9., 5.),
                    (11., 3.),
                    (13., 3.),
                    (14., 5.),
                    (13.5, 7.),
                    (11., 9.5),
                    (9., 9.5),
                ],
                &[(6., 10.), (10., 6.)],
            ],
        };
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let mut path = PathBuilder::stroke(px(1.15));
                for line in lines {
                    let (x, y) = line[0];
                    path.move_to(bounds.origin + point(px(x), px(y)));
                    for &(x, y) in &line[1..] {
                        path.line_to(bounds.origin + point(px(x), px(y)));
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .size(px(16.))
        .flex_none()
    }
}
