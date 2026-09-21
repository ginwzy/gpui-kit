use gpui_kit::component::{
    h_flex,
    theme::{ActiveTheme, Colorize},
    v_flex,
};
use gpui_kit::*;

actions!(demo, [Quit]);

struct ColorMixDemo {
    focus_handle: FocusHandle,
}

impl ColorMixDemo {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Render for ColorMixDemo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let destructive = cx.theme().danger;
        let transparent = hsla(0.0, 0.0, 0.0, 0.0);

        // 类似 CSS: color-mix(in oklab, var(--destructive) 20%, transparent)
        let mixed_20 = destructive.mix_oklab(transparent, 0.2);
        let mixed_50 = destructive.mix_oklab(transparent, 0.5);
        let mixed_80 = destructive.mix_oklab(transparent, 0.8);

        v_flex()
            .gap_4()
            .p_4()
            .track_focus(&self.focus_handle)
            .child(
                v_flex()
                    .gap_2()
                    .child("Oklab mix with transparent (使用 premultiplied alpha):")
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .size_20()
                                    .bg(destructive)
                                    .child("100%")
                                    .text_color(gpui_kit::white()),
                            )
                            .child(
                                div()
                                    .size_20()
                                    .bg(mixed_80)
                                    .child("80%")
                                    .text_color(gpui_kit::white()),
                            )
                            .child(
                                div()
                                    .size_20()
                                    .bg(mixed_50)
                                    .child("50%")
                                    .text_color(gpui_kit::white()),
                            )
                            .child(
                                div()
                                    .size_20()
                                    .bg(mixed_20)
                                    .child("20%")
                                    .text_color(gpui_kit::white()),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(format!(
                        "原始颜色: {} (alpha: {:.2})",
                        destructive.to_hex(),
                        destructive.a
                    ))
                    .child(format!(
                        "80% 混合: {} (alpha: {:.2})",
                        mixed_80.to_hex(),
                        mixed_80.a
                    ))
                    .child(format!(
                        "50% 混合: {} (alpha: {:.2})",
                        mixed_50.to_hex(),
                        mixed_50.a
                    ))
                    .child(format!(
                        "20% 混合: {} (alpha: {:.2})",
                        mixed_20.to_hex(),
                        mixed_20.a
                    )),
            )
            .child(
                v_flex()
                    .mt_4()
                    .gap_2()
                    .child("比较 HSL 和 Oklab 混合的差异 (50% transparent):")
                    .child(
                        h_flex()
                            .gap_4()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child("HSL 混合:")
                                    .child(
                                        div()
                                            .size_16()
                                            .bg(destructive.mix(transparent, 0.5))
                                            .text_color(gpui_kit::white())
                                            .child("HSL"),
                                    )
                                    .child(destructive.mix(transparent, 0.5).to_hex()),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child("Oklab 混合 (正确):")
                                    .child(
                                        div()
                                            .size_16()
                                            .bg(destructive.mix_oklab(transparent, 0.5))
                                            .text_color(gpui_kit::white())
                                            .child("Oklab"),
                                    )
                                    .child(mixed_50.to_hex()),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .mt_4()
                    .gap_2()
                    .child("说明:")
                    .child("- Oklab 混合保持了原始颜色的色调，只改变透明度")
                    .child("- HSL 混合会使颜色变暗（因为与黑色透明混合）")
                    .child("- Oklab 使用 premultiplied alpha 算法，符合 CSS color-mix 规范"),
            )
    }
}

fn main() {
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);

        cx.activate(true);
        cx.on_action(|_: &Quit, cx: &mut App| {
            cx.quit();
        });
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);

        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(600.), px(400.)), cx)),
                ..Default::default()
            },
            cx,
            |_, cx| cx.new(ColorMixDemo::new),
        )
        .expect("failed to open window");
    });
}
