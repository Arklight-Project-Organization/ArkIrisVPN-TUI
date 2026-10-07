# ArkIrisUI

一个独立的、简单直观的 ArkIris TUI 控制台。

ArkIrisUI **不实现 VPN/隧道协议**，只负责：

- 配置 ArkIris CLI 参数
- 启动 / 停止 `arkiris.exe`
- 查看运行状态与最近日志
- 分页查看连接、网络、性能、安全、配置
- 生成 X25519 静态密钥对和 32-byte PSK
- 保存/读取本地配置文件

## 页面

- `1` 总览：运行状态、模式、Peer、最近日志
- `2` 连接：启动/停止、ArkIris 可执行文件、监听/Peer
- `3` 网络：MTU、隧道网段、DNS、限速
- `4` 性能：BBR/CUBIC、FEC、LDT、pacing、privacy
- `5` 密钥：生成/查看密钥与 PSK（私钥默认隐藏）
- `6` 日志：ArkIris stdout/stderr 最近输出
- `7` 配置：查看最终生成的 CLI 命令

## 快捷键

- `1..7`：切换页面
- `Tab` / `Shift+Tab`：切换输入项
- `Enter`：编辑输入框 / 执行按钮
- `Space`：切换复选框
- `↑↓`：在选择项中切换
- `S`：保存配置
- `R`：重新加载配置
- `Ctrl+G`：生成密钥材料
- `Ctrl+S`：启动/停止 ArkIris
- `L`：清空 UI 日志
- `Q` / `Esc`：退出（运行中的 ArkIris 会被停止）

## 使用

把 `arkiris.exe` 放在 UI 旁边，或者在“连接”页面填写完整路径。

```powershell
cargo run --release
```

第一次使用建议：

1. 在“连接”页选择 `server` 或 `client`。
2. 在“密钥”页生成本机静态密钥和 PSK。
3. 服务端把客户端公钥加入 `Allowed clients`；客户端填写服务端公钥。
4. 双方 PSK 必须完全一致。
5. 回到“连接”页按 `Ctrl+S` 启动。

> ArkIris 本身仍需要正确的 TUN/Wintun 环境和管理员/系统权限（具体取决于平台）。ArkIrisUI 不绕过这些系统要求。


## ArkIris 可执行文件自动发现

启动时 ArkIrisUI 会按以下顺序查找 `arkiris.exe`：

1. Windows 注册表 `HKCU/HKLM\Software\Microsoft\Windows\CurrentVersion\App Paths\arkiris.exe`
2. ArkIrisUI 可执行文件所在目录
3. 已保存的 ArkIris 路径
4. 如果仍未找到，在“连接”页面要求用户输入 `arkiris.exe` 的完整路径

输入有效路径后会立即保存到 `arkirisui.json`，以后可以继续使用。

## Windows application resources

The project includes `ico/ArkIrisUI-Icon.ico` and `build.rs`. On Windows, the build script embeds the icon and application metadata into `arkirisui.exe`, including the product name, file description, version, internal name, and original filename.

Build with:

```powershell
cargo build --release
```
