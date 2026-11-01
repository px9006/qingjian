//! `v` 模式下的子模式路由：`v<子模式>:<语句>`。
//!
//! 冒号中文英文都认（`vbit:` 与 `vbit：` 一样）。不写子模式、或前缀认不出来时走缺省计算，
//! 与 0.3.3 之前的行为完全一致——旧的 `v1+2`、`v123`、`v1998-3-8` 一个都不受影响。

use crate::shortcut::bitwise::BitWidth;

/// 子模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode<'a> {
    /// 精确计算（分数、根式、π、e；三角函数缺省按角度）。
    Calc {
        /// 冒号之后的算式原文。
        body: &'a str,
    },

    /// 位运算，位宽与符号由子模式名给。
    Bit {
        /// 位宽与符号。
        width: BitWidth,

        /// 冒号之后的表达式原文。
        body: &'a str,
    },

    /// 直接执行 Python。
    Python {
        /// 冒号之后的源码原文。
        body: &'a str,
    },
}

/// 子模式名的几种写法：`calc` / `math` / `sym` 都算计算。
const CALC_NAMES: [&str; 3] = ["calc", "math", "sym"];

/// 拆子模式。`body` 是表达式键之后的部分（不含 `v`）。
///
/// 没有冒号时返回 `None`：那是缺省计算，交给原来的四则 / 日期 / 时间戳那套。
pub fn parse(body: &str) -> Option<Mode<'_>> {
    let (name, rest) = split_colon(body)?;
    if CALC_NAMES.contains(&name) {
        return Some(Mode::Calc { body: rest });
    }
    if name == "py" || name == "python" {
        return Some(Mode::Python { body: rest });
    }
    if let Some(width) = parse_bit_width(name) {
        return Some(Mode::Bit { width, body: rest });
    }
    None
}

/// 认 `bit` / `bitu` / `bitN` / `bitNu`；别的名字返回 `None`。
fn parse_bit_width(name: &str) -> Option<BitWidth> {
    let digits = name.strip_prefix("bit")?;
    // `bitu`：64 位无符号
    if digits == "u" {
        return Some(BitWidth {
            bits: 64,
            signed: false,
        });
    }
    let (number, signed) = match digits.strip_suffix('u') {
        Some(number) => (number, false),
        None => (digits, true),
    };
    if number.is_empty() {
        return Some(BitWidth {
            bits: 64,
            signed,
        });
    }
    let bits: u32 = number.parse().ok()?;
    matches!(bits, 8 | 16 | 32 | 64 | 128).then_some(BitWidth { bits, signed })
}

/// 在第一个冒号处切开。半角冒号、全角冒号、半角分号、全角分号都算——
/// 键盘上冒号就是 Shift + `;`，不少布局 / 输入框里送到缓冲区的是分号那一下。
fn split_colon(body: &str) -> Option<(&str, &str)> {
    let mut cut = None;
    for (index, c) in body.char_indices() {
        if matches!(c, ':' | '：' | ';' | '；') {
            cut = Some((index, c.len_utf8()));
            break;
        }
    }
    let (index, width) = cut?;
    let name = &body[..index];
    // 子模式名只能是字母数字，空的不算（`v:abc` 是缺省计算里带冒号，交给下面按普通算式判死）
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some((name, &body[index + width..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_names() {
        for name in ["calc", "math", "sym"] {
            let text = format!("{name}:sin(15)");
            match parse(&text).unwrap() {
                Mode::Calc { body } => assert_eq!(body, "sin(15)"),
                other => panic!("期望 Calc，得到 {other:?}"),
            }
        }
    }

    #[test]
    fn bit_widths() {
        let cases = [
            ("bit:1<<3", 64, true),
            ("bitu:1<<3", 64, false),
            ("bit8:0xff", 8, true),
            ("bit8u:0xff", 8, false),
            ("bit16:65535", 16, true),
            ("bit32:0xdead", 32, true),
            ("bit128:1<<100", 128, true),
        ];
        for (text, bits, signed) in cases {
            match parse(text).unwrap() {
                Mode::Bit { width, .. } => {
                    assert_eq!(width.bits, bits, "{text}");
                    assert_eq!(width.signed, signed, "{text}");
                }
                other => panic!("期望 Bit，得到 {other:?}"),
            }
        }
        assert!(parse("bit15:1").is_none());
    }

    #[test]
    fn full_width_colon_is_accepted() {
        match parse("bit8：0x1ff").unwrap() {
            Mode::Bit { width, body } => {
                assert_eq!(width.bits, 8);
                assert_eq!(body, "0x1ff");
            }
            other => panic!("期望 Bit，得到 {other:?}"),
        }
    }

    #[test]
    fn python_mode() {
        match parse("py:1+2").unwrap() {
            Mode::Python { body } => assert_eq!(body, "1+2"),
            other => panic!("期望 Python，得到 {other:?}"),
        }
    }

    #[test]
    fn unknown_prefix_is_not_a_mode() {
        assert!(parse("1+2").is_none());
        assert!(parse(":1+2").is_none());
        assert!(parse("foo:bar").is_none());
    }
}
