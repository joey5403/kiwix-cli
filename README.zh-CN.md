# kiwix-cli

[English](README.md) | [简体中文](README.zh-CN.md)

[![最新 Release](https://img.shields.io/github/v/release/joey5403/kiwix-cli?label=release)](https://github.com/joey5403/kiwix-cli/releases/latest) [![AUR 版本](https://img.shields.io/aur/version/kiwix-cli-bin?label=AUR)](https://aur.archlinux.org/packages/kiwix-cli-bin)

[![观看使用录制](https://asciinema.org/a/fVV516TzOQImj5JJ.svg)](https://asciinema.org/a/fVV516TzOQImj5JJ)

也可以使用 `asciinema play docs/kiwix-cli.cast` 在本地回放录制。

使用 Rust、Ratatui 和 Crossterm 开发的键盘优先 Kiwix 终端阅读器。默认连接 Kiwix 公共 Browse 服务，也可以配置为自建 Kiwix 服务；支持文库主页、全文搜索、随机文章、富样式文章阅读、内部链接跳转和外部图片查看，适合本地终端和 SSH 环境。

## 使用说明

### 运行要求

- 一个可访问的 Kiwix 服务
- 交互模式需要真实终端
- 从源码构建需要 Rust 1.88 或更高版本
- 查看图片需要系统文件打开器，例如 Linux 下的 `xdg-open`

### 安装

#### AUR

AUR 已提供官方 `kiwix-cli-bin` 包，可以使用 AUR 助手安装：

```bash
yay -S kiwix-cli-bin
# 或
paru -S kiwix-cli-bin
```

#### GitHub Release

下载并安装当前 Linux `x86_64` Release：

```bash
VERSION=0.1.1
ARCHIVE="kiwix-cli-${VERSION}-linux-x86_64.tar.gz"
BASE_URL="https://github.com/joey5403/kiwix-cli/releases/download/v${VERSION}"

curl --fail --location --remote-name "${BASE_URL}/${ARCHIVE}"
curl --fail --location --remote-name "${BASE_URL}/${ARCHIVE}.sha256"
sha256sum -c "${ARCHIVE}.sha256"
tar -xzf "$ARCHIVE"
install -Dm755 "kiwix-cli-${VERSION}-linux-x86_64/kiwix-cli" "$HOME/.local/bin/kiwix-cli"
install -Dm644 "kiwix-cli-${VERSION}-linux-x86_64/man/man1/kiwix-cli.1" "$HOME/.local/share/man/man1/kiwix-cli.1"
```

启动 `kiwix-cli` 前，请确保 `$HOME/.local/bin` 已加入 `PATH`。

从当前源码目录安装：

```bash
cargo install --path .
```

也可以只构建优化后的二进制：

```bash
cargo build --release
./target/release/kiwix-cli --help
```

生成包含 Linux 二进制、许可证、双语 README 和 SHA-256 校验文件的发布压缩包：

```bash
./scripts/build-linux.sh
```

安装 man 手册页：

```bash
install -Dm644 man/kiwix-cli.1 ~/.local/share/man/man1/kiwix-cli.1
man kiwix-cli
```

### 配置

默认服务地址为：

```text
https://browse.library.kiwix.org/
```

使用自建服务时，通过 `--server` 或 `KIWIX_URL` 覆盖默认地址：

```bash
export KIWIX_URL="https://wiki.example.com"
```

如果反向代理启用了 HTTP Basic Auth，需要同时设置用户名和密码：

```bash
export KIWIX_USERNAME="wiki"
export KIWIX_PASSWORD="..."
```

密码没有命令行参数，避免出现在 shell 历史和进程列表中。程序会正常校验 TLS 证书，不会自动跟随 HTTP 重定向；文库主页和随机文章的重定向会在校验后使用。

### 交互模式

不带子命令启动：

```bash
kiwix-cli
```

选中文库后按 `Enter` 打开该文库主页。需要搜索当前文库时按 `/`。

| 按键 | 操作 |
| --- | --- |
| `j` / `k`、方向键 | 移动选择或滚动文章 |
| `Enter` / `l` | 打开文库主页、搜索结果或选中的文章动作 |
| `/` | 搜索当前文库 |
| `n` / `p` | 搜索结果下一页/上一页 |
| `g` / `G` | 移到开头/结尾 |
| `Space` / `b` | 文章向下/向上翻一页 |
| `r` | 重新加载当前视图 |
| `R` | 打开当前文库中的随机文章 |
| `Tab` / `Shift-Tab` | 选择下一个/上一个文章链接或图片 |
| `f` | 为当前可见的链接和图片显示 Vimium 风格 Hint |
| 鼠标左键 | 打开指针下的链接或图片 |
| 鼠标滚轮 | 滚动文章 |
| `h` / `q` / `Esc` | 返回；在文库列表按 `q` 退出 |
| `?` | 显示上下文帮助 |
| `Ctrl-C` | 退出并恢复终端 |

文章由 `html2text` 的 rich HTML5 引擎解析，标题、强调、代码、表格、链接和图片会使用不同的终端样式。内部 Wiki 链接在当前 TUI 打开，并保留文章历史；外部链接交给系统打开器。

在文章中按 `f`，当前视口内的链接和图片会显示黄色短标签。输入标签即可打开目标，使用 `Backspace` 修正输入，或按 `Esc` 取消。

同源图片会先使用当前 Kiwix 凭据下载到会话临时目录，再启动系统图片应用，因此不会把 Basic Auth 密码传递给外部程序。固有尺寸较小的 Wiki 公式 SVG 会保留矢量路径和 `viewBox`，但以 `1400x700` 的显示画布写出。程序退出时会删除临时文件。

### 命令行模式

非交互子命令适合脚本和管道使用。

列出文库，获得 catalog UUID 和 content 名称：

```bash
kiwix-cli books
```

打开文库主页：

```bash
kiwix-cli home --content wikivoyage_en_all_maxi_2026-06
```

使用 `books` 输出的 catalog UUID 搜索：

```bash
kiwix-cli search \
  --book 12345678-1234-5678-1234-567812345678 \
  "Rust ownership"
```

读取 `search` 输出的 `/content/...` 定位符：

```bash
kiwix-cli read /content/rust_docs/A/Ownership
```

随机选择文章：

```bash
kiwix-cli random --content wikivoyage_en_all_maxi_2026-06
```

`read`、`home` 和 `random` 支持 `--width`，可以覆盖终端渲染宽度：

```bash
kiwix-cli read /content/rust_docs/A/Ownership --width 60
```

全局参数可以放在子命令之前或之后：

```text
--server <URL>       Kiwix 服务地址，或 KIWIX_URL；默认使用公共 Browse 服务
--username <NAME>    Basic Auth 用户名，或 KIWIX_USERNAME
--timeout <SECONDS>  请求超时，默认 30 秒，范围 1-300
```

使用 `kiwix-cli --help` 或 `kiwix-cli <command> --help` 查看完整命令接口。

## 开发说明

### 技术栈

- Rust 2024 edition，最低支持 Rust 1.88
- Ratatui 和 Crossterm：终端绘制与事件处理
- Reqwest 和 Rustls：由后台线程执行阻塞式 HTTPS 请求
- `html2text` rich renderer：结构化 Wiki HTML 渲染
- `quick-xml`：受限 OPDS/RSS 解析与 SVG 尺寸改写
- Clap：命令行参数解析

### 源码结构

| 路径 | 职责 |
| --- | --- |
| `src/main.rs` | CLI 定义和命令分发 |
| `src/tui.rs` | TUI 状态机、绘制、输入、历史和后台任务 |
| `src/client.rs` | 带认证的 Kiwix HTTP 客户端、URL 校验和资源获取 |
| `src/xml.rs` | OPDS 文库目录和 RSS 搜索解析器 |
| `src/article.rs` | 文章富文本、样式、链接、图片、fragment 和命中测试 |
| `src/model.rs` | 文库与搜索结果数据类型 |
| `tests/cli.rs` | 进程级 HTTP 与 CLI 集成测试 |

### Kiwix 接口映射

客户端使用以下 Kiwix 接口：

| 操作 | 请求 |
| --- | --- |
| 文库目录 | `/catalog/v2/entries?count=-1` |
| 文库主页 | `/content/{CONTENT}`，随后校验同文库重定向 |
| 搜索 | `/search?books.id={UUID}&pattern=...&start=...&pageLength=...&format=xml` |
| 随机文章 | `/random?content={CONTENT}`，随后校验同文库重定向 |
| 文章或资源 | `/raw/{CONTENT}/content/{PATH}` |

搜索和文章请求会保留配置的反向代理基础路径；文库发现使用 Kiwix 根目录下的 catalog 接口。

### 安全边界

- 凭据不会写入 URL，也不会由程序持久化。
- HTTP 自动重定向已禁用。
- 主页和随机文章的重定向必须保持在配置的同源服务和请求的文库内。
- 只有配置服务同源的文章链接会作为内部链接处理。
- 拒绝 `javascript:`、`data:`、`file:`、路径穿越、错误转义和控制字符。
- 拒绝 XML DTD 和自定义实体。
- 文本响应限制为 8 MiB，图片限制为 32 MiB。
- 带认证的图片会先下载到本地，再启动外部应用。

### 构建与验证

运行完整的本地质量门：

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release
git diff --check
```

自动化测试使用本地 mock HTTP 服务，不需要外部 Kiwix 实例。

打包脚本默认目标为 `x86_64-unknown-linux-gnu`。需要其他 Linux Rust target 时，可以通过 `TARGET` 环境变量切换到已安装的目标。

Linux 发布压缩包会把手册页放在 `man/man1/kiwix-cli.1`。

对真实服务进行手工验证：

```bash
KIWIX_URL="https://wiki.example.com" \
KIWIX_USERNAME="wiki" \
KIWIX_PASSWORD="..." \
cargo run --release
```

需要区分仓库验证与真实服务验收：测试通过只能证明本地行为，接口兼容性、外部应用启动和目标终端显示仍应在实际环境中确认。

## 许可证

MIT，参见 [LICENSE](LICENSE)。
