//! 交给壳的键盘配色（JSON 过 JNI），颜色是 Android 的 ARGB 整数。

use qingjian_render::{KeyboardColors, Theme};
use serde::Serialize;

use super::argb;

/// 字段同 [`KeyboardColors`]；壳的 `Palette` 照着读。
#[derive(Debug, Serialize)]
pub struct KeyboardPalette {
    /// 键面是深色：候选条按深色画，图标与气泡跟着用深色那一套。
    pub dark: bool,

    pub board: u32,

    pub key: u32,

    pub function_key: u32,

    pub pressed: u32,

    pub brand: u32,

    pub on_brand: u32,

    pub shadow: u32,

    pub text: u32,

    pub gloss: u32,

    pub faint: u32,

    pub fresh: u32,

    pub cloud: u32,

    /// 键的圆角（点）；`null` 时 Kotlin 用自己的缺省。
    pub key_radius: Option<f32>,

    /// 工具栏卡片与长按气泡的圆角（点）；`null` 时 Kotlin 用自己的缺省。
    pub radius: Option<f32>,
}

impl KeyboardPalette {
    pub fn new(theme: &Theme) -> Self {
        let colors = KeyboardColors::of(theme);
        Self {
            dark: colors.dark,
            board: argb(colors.board),
            key: argb(colors.key),
            function_key: argb(colors.function_key),
            pressed: argb(colors.pressed),
            brand: argb(colors.brand),
            on_brand: argb(colors.on_brand),
            shadow: argb(colors.shadow),
            text: argb(colors.text),
            gloss: argb(colors.gloss),
            faint: argb(colors.faint),
            fresh: argb(colors.fresh),
            cloud: argb(colors.cloud),
            key_radius: colors.touch_key_radius,
            radius: colors.touch_radius,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingjian_render::Theme;

    /// 方角主题（墨岩）的面板圆角必须原样传到壳：Kotlin 那边 `Ink.corner()` 就是按
    /// `palette.boardRadius == 0` 把所有面板抹成直角的。
    #[test]
    fn square_theme_reaches_the_palette() {
        let slate = Theme::builtin("slate", true).expect("内置主题里有墨岩");
        let palette = KeyboardPalette::new(&slate);
        assert_eq!(palette.radius, Some(0.0), "工具栏卡片与长按气泡的圆角");
        assert_eq!(palette.key_radius, Some(0.0), "按键的圆角");
    }

    /// 圆角主题不受影响：缺省值仍是 `None`，壳用自己的缺省。
    #[test]
    fn rounded_theme_keeps_the_shell_default() {
        let qingjian = Theme::builtin("qingjian", true).expect("内置主题里有青简");
        let palette = KeyboardPalette::new(&qingjian);
        assert!(palette.radius.is_none_or(|radius| radius > 0.0));
    }
}
