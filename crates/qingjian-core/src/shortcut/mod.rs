//! 快捷候选（搜狗「v 模式」的那一套）：不查词库、由输入本身直接算出来的候选。
//!
//! - `rq` / `sj` / `xq`：今天的日期、现在的时间、星期几，插在本地候选第二位起；打全拼出了「日期 / 时间 / 星期」
//!   这几个词（`riqi`、九宫格的 `7474`）时同样跟在这个词后面（[`keyword_forms`]）。
//! - 表达式键（缺省 `v`）开头进表达式模式：`v1+2` 出 `3` 与 `1+2=3`，`v123` 出中文数字与金额（小写与大写），
//!   `v123.5` 出小数读法与金额，同样各有小写与大写（一百二十三元五角 / 壹佰贰拾叁元伍角）；
//!   `v1998-3-8`（四位年份 + 月 + 日，分隔符 `-` / `/` / `.`）出中文日期 `一九九八年三月八日` 与
//!   `1998年3月8日` / `1998-03-08`，年份不是四位或月日不合法时回落到算式（见 [`input_date_forms`]）。
//!   表达式模式下缓冲区允许数字与运算符，候选不走拼音解析。
//! - 问字键（缺省 `u`）后跟十六进制码点出那个字符：`u4e00` → 一，`u+1f600` → 😀。模式键本身在 `engine::ModeKeys`。
//! - `vuuid`（全拼下问字键是 `u` 时直接 `uuid`）出 UUID，`vts` 出时间戳（[`generated_forms`]）；值随机或随时间变，
//!   由 Engine 按组句缓存，不走 [`candidates`]。

mod bitwise;
mod calendar;
mod evaluator;
mod modes;
mod numeral;
mod python;
mod uuid_forms;

use jiff::Zoned;

use crate::candidate::{Candidate, CandidateKind};

pub use calendar::{
    TIMESTAMP_KEYWORD, date_forms, input_date_forms, time_forms, timestamp_forms, weekday_forms,
};
pub use evaluator::evaluate;
pub use numeral::{
    amount_lower, amount_upper, chinese_decimal_lower, chinese_decimal_upper, chinese_lower,
    chinese_upper,
};
pub use uuid_forms::{UUID_KEYWORD, uuid_forms};

/// 表达式键之后整段是 `uuid` / `ts` 时生成的写法；别的词为 `None`。每次调用都重新生成，组句里的缓存在 Engine。
pub fn generated_forms(keyword: &str, now: &Zoned) -> Option<Vec<String>> {
    match keyword {
        UUID_KEYWORD => Some(uuid_forms()),
        TIMESTAMP_KEYWORD => Some(timestamp_forms(now)),
        _ => None,
    }
}

/// 是不是 [`generated_forms`] 认的词。
pub fn is_generated_keyword(keyword: &str) -> bool {
    matches!(keyword, UUID_KEYWORD | TIMESTAMP_KEYWORD)
}

/// 表达式模式的缺省前缀。`v` 不是任何拼音音节的开头，用它不会和拼音冲突。
pub const EXPRESSION_PREFIX: char = 'v';

/// 码点最长几位十六进制（U+10FFFF）。
const MAX_CODEPOINT_DIGITS: usize = 6;

/// 问字前缀之后的部分是不是 Unicode 码点：`4e00`、`+1f600`。全是十六进制且至少含一个数字，
/// 或以 `+` 开头（`+face` 这种纯字母也算）；纯字母不带 `+` 当拼音问题（`fade` 是合法拼音）。
/// 是码点就返回那个字符（不可显示的控制字符、代理区、超范围返回 `None`）。
pub fn unicode_form(body: &str) -> Option<String> {
    let (explicit, hex) = match body.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    if hex.is_empty()
        || hex.len() > MAX_CODEPOINT_DIGITS
        || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        || (!explicit && !hex.bytes().any(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let c = char::from_u32(value)?;
    (!c.is_control()).then(|| c.to_string())
}

/// 到目前为止敲的还可能是码点（空、`+`、或全是十六进制）：壳据此决定数字进缓冲区还是选词。
pub fn could_be_unicode(body: &str) -> bool {
    let hex = body.strip_prefix('+').unwrap_or(body);
    hex.len() <= MAX_CODEPOINT_DIGITS && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 表达式模式下允许敲进缓冲区的非字母字符：数字、四则运算符号，以及子模式要用的冒号与逗号。
/// 字母（`x` 当乘号、`sin` 这种函数名）本来就能进缓冲区，不在此列。
pub fn is_expression_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '+' | '-' | '*' | '/' | '(' | ')' | '.' | '^' | ':' | ',' | '%' | ';' | '：')
}

/// 屏幕键盘上的 `c` 在算式里当什么：乘除号（`×` `÷`）换成 `*` `/`，其余算式字符原样，不是算式字符为 `None`。
pub fn expression_char(c: char) -> Option<char> {
    let c = match c {
        '×' => '*',
        '÷' => '/',
        c => c,
    };
    is_expression_char(c).then_some(c)
}

/// 按输入算快捷候选；不是快捷输入时为空。`expression` 是表达式键；`now` 由调用方给，测试可固定时间。
pub fn candidates(input: &str, expression: char, now: &Zoned) -> Vec<Candidate> {
    candidates_with_modes(input, expression, now, true)
}

/// 同 [`candidates`]，`sub_modes` 决定认不认 `vcalc:` / `vbit:` / `vpy:` 这些子模式。
///
/// 子模式只在全拼下开着：双拼、注音、形码里缓冲区里的字母另有含义（`;` 是双拼的 ing 键、
/// 是辅码触发键），把带冒号的串当子模式解析会把正常输入吃掉。
pub fn candidates_with_modes(
    input: &str,
    expression: char,
    now: &Zoned,
    sub_modes: bool,
) -> Vec<Candidate> {
    let texts: Vec<String> = match input {
        "rq" => date_forms(now),
        calendar::TIME_KEYWORD => time_forms(now),
        "xq" => weekday_forms(now),
        _ => match input.strip_prefix(expression) {
            Some(body) => expression_forms(body, sub_modes),
            None => Vec::new(),
        },
    };
    as_candidates(texts)
}

/// 这几个词的候选**随时间变**（`sj` 的秒与毫秒）：壳的定时节拍里要重查，
/// 否则组句停着不动时那几位就冻在查询那一刻。`rq` / `xq` 一天才变，不在其列。
///
/// 判断放 Core 是因为三个壳都已经有节拍（Windows 的 DLL Poll 80 ms、macOS 的 NSTimer、
/// Linux 插件的 Poll），共同的缺口只是「什么时候该重查」——各壳问一句就够。
pub fn is_live_time_keyword(text: &str) -> bool {
    text == calendar::TIME_KEYWORD
}

/// 快捷写法包成候选。
pub(crate) fn as_candidates(texts: Vec<String>) -> Vec<Candidate> {
    texts
        .into_iter()
        .map(|text| Candidate {
            text,
            kind: CandidateKind::Shortcut,
            syllables: Vec::new(),
            reading: None,
            translation: None,
            aux_code: None,
        })
        .collect()
}

/// 打全拼出的词是「日期 / 时间 / 星期」时对应的快捷写法，同 `rq` / `sj` / `xq`；别的词为空。
pub fn keyword_forms(word: &str, now: &Zoned) -> Vec<String> {
    match word {
        "日期" => date_forms(now),
        "时间" => time_forms(now),
        "星期" => weekday_forms(now),
        _ => Vec::new(),
    }
}

/// 金额最多几位小数（角、分）。
const AMOUNT_FRACTION_DIGITS: usize = 2;

/// 表达式键之后的部分：一个数出中文数字与金额，四位年份加月日的日期出日期写法，其余当四则运算出结果与「算式=结果」。
/// 顺序是要紧的：`1998` 与 `1998.3` 是数字与小数，先于日期；四位年份的 `1998-3-8` 才当日期，落在算式之前。
fn expression_forms(body: &str, sub_modes: bool) -> Vec<String> {
    // 子模式优先：`calc:` / `bit:` / `py:` 这些带冒号的，认出来就只走那一条，不再按四则去猜
    if sub_modes
        && let Some(mode) = modes::parse(body)
    {
        return mode_forms(mode);
    }
    if let Some(forms) = number_forms(body) {
        return forms;
    }
    if let Some(forms) = input_date_forms(body) {
        return forms;
    }
    match evaluate(body) {
        Some(result) => vec![result.clone(), format!("{body}={result}")],
        None => Vec::new(),
    }
}

/// 子模式的候选。
fn mode_forms(mode: modes::Mode<'_>) -> Vec<String> {
    match mode {
        modes::Mode::Calc { body } => calc_forms(body),
        modes::Mode::Bit { width, body } => {
            let Some(result) = bitwise::evaluate(body, width) else {
                return Vec::new();
            };
            let forms = result.forms();
            // 与老四则的 `1+2=3` 同一套：第二条就是「敲的式子 = 结果」
            match forms.first() {
                Some(first) if !body.contains('=') && body.trim() != first.as_str() => {
                    let mut out = vec![first.clone(), format!("{body}={first}")];
                    out.extend(forms.into_iter().skip(1));
                    out
                }
                _ => forms,
            }
        }
        modes::Mode::Python { body } => python::run_python(body)
            .filter(|out| !out.is_empty())
            .map(|out| vec![out])
            .unwrap_or_default(),
    }
}

/// 精确计算的候选：精确形式、「式子 = 结果」、LaTeX、数值近似、中文小写与大写、分数读法。
/// 同一条文字只留一次（大整数时精确形式与数值形式往往一模一样）。
fn calc_forms(body: &str) -> Vec<String> {
    let Some(output) = python::calculate(body) else {
        return Vec::new();
    };
    let mut forms: Vec<String> = Vec::new();
    let mut push = |text: String| {
        if !text.is_empty() && !forms.contains(&text) {
            forms.push(text);
        }
    };
    push(output.exact.clone());
    // 与老四则 `1+2=3` 一致：第二条是「敲的式子 = 结果」
    if !output.exact.is_empty() && !body.contains('=') && body.trim() != output.exact {
        push(format!("{body}={}", output.exact));
    }
    // 输入式子本身的 LaTeX：转格式用，`sqrt(2)/2` → `\frac{\sqrt{2}}{2}`
    if output.latex_input != output.latex {
        push(output.latex_input.clone());
    }
    push(output.latex.clone());
    // 整数时数值形式（`1234.000000000000`）没信息，不要
    if output.integer.is_none() {
        push(output.numeric.clone());
    }
    // 整数给中文大小写：这是输入法里最常要的一种「换个写法」
    if let Some(integer) = &output.integer {
        push(numeral::chinese_lower(integer));
        push(numeral::chinese_upper(integer));
    }
    if let Some((numerator, denominator)) = &output.fraction
        && denominator != "1"
    {
        push(format!("{denominator}分之{numerator}"));
        push(format!(
            "{}分之{}",
            numeral::chinese_upper(denominator),
            numeral::chinese_upper(numerator)
        ));
    }
    forms
}

/// 单个非负数（`123`、`123.5`）：中文数字在前，金额在后；小数超过两位没有金额写法。
/// 末尾刚敲了 `.`（`123.`）按整数给，候选不闪。
fn number_forms(body: &str) -> Option<Vec<String>> {
    let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
    let digits = |text: &str| text.bytes().all(|b| b.is_ascii_digit());
    if integer.is_empty() || !digits(integer) || !digits(fraction) {
        return None;
    }
    let mut forms = if fraction.is_empty() {
        vec![chinese_lower(integer), chinese_upper(integer)]
    } else {
        vec![
            chinese_decimal_lower(integer, fraction),
            chinese_decimal_upper(integer, fraction),
        ]
    };
    if fraction.len() <= AMOUNT_FRACTION_DIGITS {
        forms.push(amount_lower(integer, fraction));
        forms.push(amount_upper(integer, fraction));
    }
    Some(forms)
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    use super::*;

    #[test]
    fn unicode_bodies_need_a_digit_or_a_plus() {
        assert_eq!(unicode_form("4e00").as_deref(), Some("一"));
        assert_eq!(unicode_form("+1f600").as_deref(), Some("😀"));
        assert_eq!(unicode_form("+face").as_deref(), Some("\u{face}"));
        // 纯字母没有 + 当拼音问题（fade 是合法拼音）
        assert_eq!(unicode_form("fade"), None);
        assert_eq!(unicode_form(""), None);
        assert_eq!(unicode_form("+"), None);
        // 控制字符、代理区、超范围
        assert_eq!(unicode_form("07"), None);
        assert_eq!(unicode_form("d800"), None);
        assert_eq!(unicode_form("110000"), None);
        assert_eq!(unicode_form("1234567"), None);
        assert!(could_be_unicode(""));
        assert!(could_be_unicode("+"));
        assert!(could_be_unicode("4e"));
        assert!(could_be_unicode("face"));
        assert!(!could_be_unicode("sangemu"));
    }

    fn now() -> Zoned {
        date(2026, 9, 3)
            .at(19, 6, 23, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
    }

    fn texts(input: &str) -> Vec<String> {
        candidates(input, EXPRESSION_PREFIX, &now())
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn calendar_shortcuts() {
        assert_eq!(texts("rq"), ["2026年9月3日", "2026-09-03", "2026/09/03"]);
        assert_eq!(
            texts("sj"),
            ["19:06", "19:06:23", "19点06分", "20260903190623"]
        );
        assert_eq!(texts("xq"), ["星期四", "周四"]);
        assert!(texts("rqi").is_empty());
    }

    #[test]
    fn timestamp_forms_cover_unix_and_rfc3339() {
        assert_eq!(
            timestamp_forms(&now()),
            [
                "1788462383",
                "2026-09-03T19:06:23Z",
                "1788462383000",
                "2026-09-03T19:06:23+00:00"
            ]
        );
        let beijing = now().with_time_zone(TimeZone::fixed(jiff::tz::offset(8)));
        assert_eq!(timestamp_forms(&beijing)[1], "2026-09-03T19:06:23Z");
        assert_eq!(timestamp_forms(&beijing)[3], "2026-09-04T03:06:23+08:00");
        assert!(is_generated_keyword("ts") && is_generated_keyword("uuid"));
        assert!(!is_generated_keyword("t"));
    }

    #[test]
    fn keyword_shortcuts() {
        assert_eq!(keyword_forms("日期", &now()), texts("rq"));
        assert_eq!(keyword_forms("时间", &now()), texts("sj"));
        assert_eq!(keyword_forms("星期", &now()), texts("xq"));
        assert!(keyword_forms("日起", &now()).is_empty());
    }

    #[test]
    fn expression_shortcuts() {
        assert_eq!(texts("v1+2"), ["3", "1+2=3"]);
        assert_eq!(
            texts("v123"),
            [
                "一百二十三",
                "壹佰贰拾叁",
                "一百二十三元整",
                "壹佰贰拾叁元整"
            ]
        );
        assert_eq!(
            texts("v123.5"),
            [
                "一百二十三点五",
                "壹佰贰拾叁点伍",
                "一百二十三元五角",
                "壹佰贰拾叁元伍角"
            ]
        );
        assert_eq!(texts("v123."), texts("v123"));
        // 超过角分的小数只有读法；带运算符的照常算
        assert_eq!(texts("v3.14159"), ["三点一四一五九", "叁点壹肆壹伍玖"]);
        assert_eq!(texts("v1.5*2"), ["3", "1.5*2=3"]);
        assert!(texts("v").is_empty());
        assert!(texts("v1+").is_empty());
        assert!(texts("very").is_empty());
    }

    #[test]
    fn date_inputs_in_expression_mode() {
        assert_eq!(
            texts("v1998-3-8"),
            ["一九九八年三月八日", "1998年3月8日", "1998-03-08"]
        );
        // `-` / `/` / `.` 都当分隔符，补零也认
        assert_eq!(texts("v1998/3/8")[0], "一九九八年三月八日");
        assert_eq!(texts("v2006.12.25")[0], "二〇〇六年十二月二十五日");
        assert_eq!(texts("v1998-03-08")[0], "一九九八年三月八日");
        // 年份不是四位、段数不对、月日不合法 → 仍按算式
        assert_eq!(texts("v1-2-3"), ["-4", "1-2-3=-4"]);
        assert_eq!(texts("v98-3-8"), ["87", "98-3-8=87"]);
        assert_eq!(texts("v1998-2-30"), ["1966", "1998-2-30=1966"]);
        assert_eq!(texts("v1998-3-8-1"), ["1986", "1998-3-8-1=1986"]);
        // 数字与小数写法不受日期影响
        assert_eq!(texts("v1998")[0], "一千九百九十八");
        assert_eq!(texts("v1998.3")[0], "一千九百九十八点三");
    }

    #[test]
    fn expression_chars() {
        assert!(is_expression_char('7'));
        assert!(is_expression_char('('));
        assert!(!is_expression_char('='));
        assert!(!is_expression_char('x'));
        assert_eq!(expression_char('×'), Some('*'));
        assert_eq!(expression_char('÷'), Some('/'));
        assert_eq!(expression_char('3'), Some('3'));
        assert_eq!(expression_char('='), None);
    }
}
