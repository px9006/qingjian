//! `vcalc` / `vpy` 子模式的后端：一个常驻的 Python 子进程。
//!
//! 为什么是常驻：精确计算（分数、根式化简、π、e、LaTeX）这套在 Rust 生态里没有现成的符号引擎，
//! 而 Python 的 sympy 是这件事的事实标准。`python -c` 每键起一次进程要 100–200 ms，打字时不能接受，
//! 所以起一次、走行协议、按表达式缓存。
//!
//! 边界：
//! - 子进程拿不到时（机器上没有 python、或 sympy 没装）所有 `vcalc` / `vpy` 候选直接不出，
//!   缺省计算（四则、日期、时间戳）不受影响。
//! - 每次请求有超时（[`TIMEOUT`]），超时或进程挂了就丢弃并允许下次重启，输入本身不阻塞更久。
//! - 缓存只按表达式原文存，条数有上限（[`CACHE_LIMIT`]），不做 LRU：满了整个清掉重来，够用且简单。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

/// 一次请求最多等多久。sympy 化简复杂式子可能慢，但输入法不能等太久。
const TIMEOUT: Duration = Duration::from_millis(900);

/// 子进程刚起来那一次的等待上限：`import sympy` 本身要一两秒，用 [`TIMEOUT`] 会把第一问直接判死。
const FIRST_TIMEOUT: Duration = Duration::from_secs(6);

/// 缓存条数上限。
const CACHE_LIMIT: usize = 256;

/// 内嵌的后端脚本：一行 JSON 请求进，一行 JSON 应答出。
///
/// 角度是缺省的：`sin(15)` 按 15° 算（`deg()` 折成弧度）；写了 `rad` 或含 `pi` 的按弧度原样算。
const BACKEND: &str = r#"
import sys, json, re
import sympy as sp
from sympy.parsing.sympy_parser import (parse_expr, standard_transformations,
                                        implicit_multiplication_application, convert_xor)

TRANSFORMS = standard_transformations + (implicit_multiplication_application, convert_xor)
TRIG = r'(sin|cos|tan|cot|sec|csc)'
TRIG_RE = re.compile(r'\b' + TRIG + r'\s*\(([^()]*)\)')

def to_radians(text):
    """三角函数里的裸数字按角度算；带 pi 或写成 rad 的按弧度。"""
    def fix(match):
        name, arg = match.group(1), match.group(2)
        if 'pi' in arg or 'rad' in arg:
            return f'{name}({arg})'
        return f'{name}(deg({arg}))'
    return TRIG_RE.sub(fix, text)

def calc(expr):
    source = to_radians(expr.strip())
    local = {'deg': lambda x: sp.sympify(x) * sp.pi / 180,
             'pi': sp.pi, 'e': sp.E, 'E': sp.E}
    value = parse_expr(source, local_dict=local, transformations=TRANSFORMS,
                       evaluate=True)
    exact = sp.simplify(value)
    # 合并成单一分式，`-sqrt(2)/4 + sqrt(6)/4` 才写成 `(sqrt(6)-sqrt(2))/4`
    try:
        merged = sp.simplify(sp.together(exact))
    except Exception:
        merged = exact
    numeric = sp.N(merged, 16)
    out = {
        'exact': str(merged),
        'latex': sp.latex(merged),
        'numeric': str(numeric),
        'is_integer': bool(merged.is_Integer),
        'is_rational': bool(merged.is_Rational),
    }
    if merged.is_Rational:
        out['numerator'] = str(merged.p)
        out['denominator'] = str(merged.q)
    return out

def run_py(code):
    import io, contextlib
    buffer = io.StringIO()
    with contextlib.redirect_stdout(buffer):
        try:
            # 当表达式先算：REPL 的手感，`1+2` 直接出 3
            value = eval(compile(code, '<vpy>', 'eval'), {'__name__': '__vpy__'})
            if value is not None:
                print(repr(value))
        except SyntaxError:
            exec(compile(code, '<vpy>', 'exec'), {'__name__': '__vpy__'})
    return {'out': buffer.getvalue()}

def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except Exception as error:
            print(json.dumps({'ok': False, 'error': f'bad request: {error}'}), flush=True)
            continue
        try:
            mode = request.get('mode')
            if mode == 'calc':
                payload = calc(request.get('body', ''))
            elif mode == 'py':
                payload = run_py(request.get('body', ''))
            else:
                payload = {}
                raise ValueError(f'unknown mode {mode!r}')
            print(json.dumps({'ok': True, 'data': payload}, ensure_ascii=False), flush=True)
        except Exception as error:
            print(json.dumps({'ok': False, 'error': str(error)}, ensure_ascii=False), flush=True)

main()
"#;

/// 一次计算的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CalcOutput {
    /// 精确形式（sympy 的 `str`），如 `(sqrt(6) - sqrt(2))/4`。
    pub exact: String,

    /// LaTeX 形式，如 `\frac{- \sqrt{2} + \sqrt{6}}{4}`。
    pub latex: String,

    /// 数值近似。
    pub numeric: String,

    /// 结果是整数时给中文数字用的十进制原文。
    pub integer: Option<String>,

    /// 结果是分数时给「分子/分母」。
    pub fraction: Option<(String, String)>,
}

/// Python 子进程；连不上或中途挂了时为 `None`。
struct Bridge {
    /// 子进程句柄，`Drop` 时杀掉。
    child: Child,

    /// 往子进程写请求。
    stdin: ChildStdin,

    /// 读子进程的逐行应答。
    replies: Receiver<String>,

    /// 已经成功答过一次；第一次用 [`FIRST_TIMEOUT`]。
    warmed: bool,
}

impl Bridge {
    fn spawn() -> Option<Self> {
        let mut child = Command::new(python_command())
            .args(["-u", "-c", BACKEND])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let (tx, replies) = mpsc::channel();
        thread::Builder::new()
            .name("qingjian-vpy-reader".to_owned())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            if tx.send(line).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
            .ok()?;
        Some(Self {
            child,
            stdin,
            replies,
            warmed: false,
        })
    }

    fn request(&mut self, payload: &str) -> Option<String> {
        writeln!(self.stdin, "{payload}").ok()?;
        self.stdin.flush().ok()?;
        let wait = if self.warmed { TIMEOUT } else { FIRST_TIMEOUT };
        match self.replies.recv_timeout(wait) {
            Ok(line) => {
                self.warmed = true;
                Some(line)
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                // 超时或断了：下次重新起一个，别让卡死的进程一直占着
                self.warmed = false;
                None
            }
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 进程级的那一个桥；没起来过就是 `None`。
fn bridge() -> &'static Mutex<Option<Bridge>> {
    static BRIDGE: OnceLock<Mutex<Option<Bridge>>> = OnceLock::new();
    BRIDGE.get_or_init(|| Mutex::new(None))
}

/// 缓存：表达式原文 → 结果。
fn cache() -> &'static Mutex<Vec<(String, Option<CalcOutput>)>> {
    static CACHE: OnceLock<Mutex<Vec<(String, Option<CalcOutput>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// 找 Python：先看环境变量 `QINGJIAN_PYTHON`，再按平台试常见名字。
fn python_command() -> String {
    if let Some(explicit) = std::env::var_os("QINGJIAN_PYTHON") {
        return explicit.to_string_lossy().into_owned();
    }
    if cfg!(windows) {
        "python".to_owned()
    } else {
        "python3".to_owned()
    }
}

/// 发一条请求，返回 `data` 字段的 JSON 对象。
fn call(mode: &str, body: &str) -> Option<serde_json::Value> {
    let request = serde_json::json!({ "mode": mode, "body": body }).to_string();
    let mut guard = bridge().lock().ok()?;
    if guard.is_none() {
        *guard = Bridge::spawn();
    }
    let reply = guard.as_mut()?.request(&request)?;
    let parsed: serde_json::Value = serde_json::from_str(&reply).ok()?;
    if parsed.get("ok")?.as_bool()? {
        Some(parsed.get("data")?.clone())
    } else {
        // 解析失败（用户还没敲完）时不该让桥继续占着，下一次照样能问
        None
    }
}

/// 精确计算。`body` 是 `vcalc:` 之后的部分。
pub fn calculate(body: &str) -> Option<CalcOutput> {
    let key = format!("calc:{body}");
    if let Some(hit) = lookup(&key) {
        return hit;
    }
    let data = call("calc", body);
    let output = data.map(|data| {
        let field = |name: &str| data.get(name).and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let integer = data
            .get("is_integer")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            .then(|| field("exact"));
        let fraction = data
            .get("is_rational")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            .then(|| (field("numerator"), field("denominator")));
        CalcOutput {
            exact: field("exact"),
            latex: field("latex"),
            numeric: field("numeric"),
            integer,
            fraction,
        }
    });
    remember(key, output.clone());
    output
}

/// 执行一段 Python，返回标准输出（去掉末尾换行）。
pub fn run_python(body: &str) -> Option<String> {
    let key = format!("py:{body}");
    if let Some(hit) = lookup(&key) {
        return hit.map(|output| output.exact);
    }
    let output = call("py", body)
        .and_then(|data| data.get("out").and_then(|v| v.as_str()).map(str::to_owned))
        .map(|out| out.trim_end_matches(['\n', '\r']).to_owned());
    remember(
        key,
        output.clone().map(|out| CalcOutput {
            exact: out,
            ..CalcOutput::default()
        }),
    );
    output
}

fn lookup(key: &str) -> Option<Option<CalcOutput>> {
    let guard = cache().lock().ok()?;
    guard
        .iter()
        .find(|(stored, _)| stored == key)
        .map(|(_, value)| value.clone())
}

fn remember(key: String, value: Option<CalcOutput>) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    if guard.len() >= CACHE_LIMIT {
        guard.clear();
    }
    guard.push((key, value));
}
