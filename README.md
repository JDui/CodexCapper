# CodexCapper 2.0.0

Codex 额度桌面组件。用进度条实时显示 Codex 账号每个额度窗口（5 小时 / 每周）的剩余比例和距重置的倒计时，可以悬浮在桌面上，也可以嵌进 Windows 任务栏常驻，一眼看到额度还剩多少。

数据不走任何第三方服务器：直接读取本机 `~/.codex` 的会话与凭证，并在本地拉起 `codex` CLI 查询官方额度接口，全程无网络服务、无账号体系、无遥测。首次启动即嵌在任务栏右端、贴着托盘图标，不占屏幕一寸地方。

## 2.0.0 静态截图示例

以下均为 2.0.0 实际运行截图，使用合成示例额度；图片只保留组件本身，不包含其他应用窗口。

| 原生任务栏 · 单行紧凑 | 悬浮 · 单行紧凑 |
|:---:|:---:|
| ![任务栏单行紧凑模式，仅显示实际存在的 7d 额度](docs/media/taskbar.png) | ![悬浮单行紧凑模式](docs/media/compact.png) |

| 完整悬浮 · 深色 · 仅 7d | 完整悬浮 · 浅色 · 5h 与 7d |
|:---:|:---:|
| ![Pro Lite 深色主题，仅显示 7d](docs/media/float-dark.png) | ![Plus 浅色主题，显示 5h 与 7d](docs/media/float-light.png) |

紧凑模式是单行仪表：只显示最紧张的额度窗口、进度条、百分比和两项倒计时，订阅标题不再单独占一行。悬浮卡约 `154×24`，任务栏组件约 `140×20` 逻辑像素；尺寸随系统 DPI 缩放。悬浮紧凑条整行可拖动，右键打开设置。

![额度预警](docs/media/states.png)

预警示例中的 7d 剩余 7%，百分比变红；5h 保持蓝绿色、7d 保持雾紫色，方便辨认窗口。主题也可跟随系统自动切换。

## 特性

2.0.0 的外观参考 `chatgpt-model-meter`：订阅名标题、圆角灰色卡片、蓝绿 / 雾紫渐变额度条，以及短格式重置倒计时。任务栏模式使用同一套配色的原生绘制。

- **按账号实际窗口显示**：只有每周额度的 Pro / Pro Lite 账号显示一条 `7d`，有两个窗口时分别显示；窗口名称按官方返回的时长确定，支持新版 `rateLimitsByLimitId`。显示剩余比例、进度条与重置时间，估算数据带「估」角标
- **两种摆放方式**
  - **悬浮**：无边框圆角小窗，可拖动，默认贴在屏幕右下角；纯覆盖，不占用屏幕任何位置，也不要求别的窗口让开
  - **嵌入任务栏**（仅 Windows）：创建原生 `WS_CHILD` 窗口，并通过 XAML Diagnostics 布局桥接在 Win11 的任务标签容器中预留宽度，让应用标签在剩余空间内排布。收到实际布局缩小的确认、并检查按钮不与槽位重叠后，才使用该位置。隐藏、切换到悬浮或退出时恢复标签容器；进程异常结束时也由租约自动恢复。桥接不可用时，才退回测量空位、避让到另一端的兼容方式；两端均满则使用任务栏旁的预留条带，该条带会按高度占用整屏宽度的工作区
- **托盘图标即状态灯**：按最紧张窗口的使用率变色——绿色正常、琥珀色（≥70%）提醒、红色（≥90%）告急，不用点开就能看出额度是否吃紧
- **全中文托盘菜单**：立即刷新、强制重启、显示组件、刷新频率（1/3/5/15 分钟）、位置、显示器、外观（主题 / 紧凑模式 / 鼠标穿透 / 全屏应用时隐藏 / 重置时间样式）、开机自动启动、退出
- **鼠标穿透**：让点击落到组件下面的窗口上，适合把组件当"仪表盘"常驻的场景
- **全屏应用时自动隐藏**：打游戏或演示时不打扰，也不会把独占全屏的游戏挤出全屏
- **回满庆祝**：某个额度窗口重置回满的瞬间，整条进度条会短暂滚动一道彩虹
- **开机自动启动**：走注册表 Run 键，绿色免安装
- **本地化数据源**：只读 `~/.codex`（sessions / auth.json / config.toml），通过本地 `codex` CLI 的 `initialize` 握手获取官方 rate_limits；拿不到官方数据时退化为按本地会话记录估算，并在界面上明确标注

## 下载与安装

前往 [v2.0.0 Release](https://github.com/JDui/CodexCapper/releases/tag/v2.0.0) 下载：

- [Windows x64 安装包](https://github.com/JDui/CodexCapper/releases/download/v2.0.0/CodexCapper_2.0.0_x64-setup.exe)：当前用户安装，无需管理员权限。
- [Windows x64 便携版](https://github.com/JDui/CodexCapper/releases/download/v2.0.0/CodexCapper-2.0.0-win-x64-portable.zip)：解压后运行 `codexcapper.exe`，无需安装。

首次运行后组件默认嵌在任务栏右端、紧贴托盘图标；想改成悬浮窗口，右键系统托盘里的 CodexCapper 图标，在「位置」菜单里切换即可。

### 运行前提

- Windows 10 / 11（Linux 也支持，见下文构建）
- 本机已安装并登录 Codex CLI（`codex login`）。如果装过 ChatGPT 桌面版，组件会自动使用其自带的 codex 二进制，无需单独安装 CLI

## 从源码构建

需要 [Node.js](https://nodejs.org/)（配 pnpm）、[Rust](https://www.rust-lang.org/tools/install) 和 Windows 上的 [Microsoft Visual Studio C++ 生成工具](https://visualstudio.microsoft.com/visual-cpp-build-tools/)：

```sh
# 安装依赖并构建前端（tsc 类型检查 + vite 打包到 dist/）
pnpm install
pnpm run build

# 构建后端（release 编译，产物在 src-tauri/target/release/codexcapper.exe）
cd src-tauri
cargo build --release
```

前端资源内嵌进可执行文件，编译出的 `codexcapper.exe` 单文件即可运行。

日常开发热更新：

```sh
pnpm run app:dev
```

## 配置

配置保存在 `%APPDATA%\CodexCapper\settings.json`（Linux 为 `~/.config/CodexCapper/`），一般无需手改，托盘菜单里的选项都会即时写入。常用项：

| 配置 | 说明 |
|---|---|
| 刷新频率 | 官方额度查询间隔，1 / 3 / 5 / 15 分钟 |
| 位置 | 悬浮 / 固定到任务栏（靠左端或靠右端） |
| 显示器 | 多显示器时指定组件在哪块屏上 |
| 主题 | 跟随系统 / 深色 / 浅色 |
| 紧凑模式 | 单行显示最紧张的窗口 |
| 鼠标穿透 | 点击穿过组件 |
| 全屏应用时隐藏 | 检测到全屏应用时收起 |
| 重置时间 | 倒计时（如 `1h 6m`）或绝对时间样式 |
| 开机自动启动 | 注册表 Run 键，免管理员权限 |

调试模式：用 `codexcapper.exe --demo` 启动可展示演示数据，不读取真实账号。

如果之前手动隐藏过组件，可用 `codexcapper.exe --show` 或托盘菜单中的「显示组件」恢复。任务栏模式与悬浮模式共用同一个显示开关。

Windows 11 的标签区由 XAML 绘制，进程外移动普通窗口无法为它预留容量。布局桥接会通过微软的 XAML Diagnostics 接口向 Explorer 加载本软件自带的 DLL，不需要安装 Windhawk。桥接只管理自己写入的边距；主程序停止后恢复布局，诊断 DLL 按接口生命周期保持休眠，直到 Explorer 结束。桥接二进制嵌在 EXE 内，运行时释放到用户配置目录的版本化路径。此方式不是正式的第三方任务栏扩展接口，系统升级后需重新验证兼容性。

## 开发说明

- 前端：React + TypeScript + Vite + Tailwind CSS
- 后端：Rust + Tauri v2，托盘、任务栏嵌入、全屏检测均为原生实现
- 测试：`cargo test --lib`（Rust 侧单元测试）
- `scripts/` 下有文档截图的重拍脚本

任务栏布局验证：`scripts/verify-reservation.ps1 -RequireApplied` 独立检查实际容量缩小量和确认代数；`scripts/verify-taskbar.ps1` 检查原生父子窗口、分层绘制和可见性。2026-10-29 在 Windows 11 26200 上实测：预留 195 物理像素后，标签容器有效宽度由 5120 降为 4925；填满到 25 个标签时，常见标签宽度由 175 降为 166。结束进程后，预留边距自动恢复。紧凑模式实际为任务栏 `140×20`、悬浮 `154×24` 逻辑像素。

## 2.0.0 更新

- Win11 任务栏优先为组件预留应用标签区宽度，应用标签随之压缩排布；空位避让保留为兼容回退。
- 紧凑模式改为单行，去掉单独占行的订阅标题；参考 `chatgpt-model-meter` 更新圆角卡片和蓝绿／雾紫渐变。
- 修复 Pro / Pro Lite 只有 `7d` 窗口时的显示，并兼容 `rateLimitsByLimitId`。
- 隐藏、切换模式和退出时恢复标签布局；新增原生窗口与布局预留验证脚本，并更新全部静态截图。

## 许可

[GPL-3.0-or-later](LICENSE)
