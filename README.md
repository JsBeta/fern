# fern

一个单文件、零第三方依赖的 HTTP 文件服务器：Rust 标准库实现，自带前端、JSON API、回收站与流式 ZIP，配置与命令行均为本项目自有设计。

拷一个可执行文件到目标机器，双击就跑：没有配置文件时直接共享 `fern.exe` 所在目录，端口 `4545`，访客只读、可列目录，浏览器打开即用。没有数据库、没有运行时、没有安装包，也没有一行第三方 crate。

## 主要功能

| 分类 | 说明 |
|---|---|
| 浏览与下载 | 目录列表、面包屑、键盘多选、深色/浅色自适应；工具栏吸顶，长列表滚动时操作按钮始终可点；工具栏为内联 SVG 纯图标 + 悬停气泡提示（随状态切换图标与文案）；`/<path>` 直链下载，目录 `302` 到前端路由 |
| 上传 | 拖拽上传、整目录上传、分块断点续传（`.fernpart` 分片，刷新或重试后自动续传）、取消即清理残留 |
| 文件管理 | 新建文件夹/空文件、重命名、复制（目录递归，重名自动「- 副本 (2)」）、剪切移动、删除 |
| 在线编辑 | UTF-8 文本读写，保留 BOM 与原权限位；`mtimeMs` 冲突检测，支持强制覆盖 |
| 回收站 | 目录式 `.Trash/<id>/{meta.json,data/}`：浏览、恢复（可覆盖）、彻底删除、清空，按天数/同名份数/总容量自动淘汰 |
| 打包下载 | 目录流式 ZIP，存储式写入，`Content-Length` 在响应头即可知，边打包边发送 |
| 访问控制 | `RLUMSTD` 字母权限，按用户与访客分别配置；登录失败限流、`HttpOnly` + `SameSite=Lax` 会话 |
| 配置 | [`fern.conf`](./fern.conf.example)（`Fern.*` + `Trash.*`）、命令行参数、环境变量覆盖、[`fern.mime`](./fern.mime.example) 扩展 MIME 表 |
| 平台集成 | Windows 托盘图标、服务注册、防火墙规则；Unix `rlimit` / `chroot` / `setuid` / `syslog` |
| 运行辅助 | 按天日志、界面标题与页头通知、`--check` 校验配置并打印实际生效值 |

## 亮点

- **真的零依赖**：`[dependencies]` 为空。HTTP、JSON、URL 编解码、HTTP 日期、base36、MIME、Range、ZIP、gzip/deflate、日志全部手写，构建时不需要联网拉取任何 crate。
- **体积优先**：`opt-level="z"` + fat LTO + `strip` + `panic="abort"`，Windows x86_64 产物约 **625 KiB**（640 000 字节），armv7 完全静态约 **1.03 MiB**；前端 4 个静态文件在构建期压成 gzip（58 KB → 19 KB）内嵌进二进制，首次请求时解压并缓存。
- **无 async runtime**：`TcpListener` + 每连接一线程（默认上限 128、线程栈 128 KiB），调用栈清晰、便于调试，也方便在 Windows 7 这类老系统上做静态链接。
- **协议细节齐全**：`Range` / `416` / `ETag` / `304`、`Expect: 100-continue`、HTTP/1.1 keep-alive（每连接 100 请求上限）、目录流式 ZIP、上传断点续传。
- **安全默认**：路径穿越拦截（`..`、`%2e%2e`、`..%5c`、符号链接逃逸）、登录失败限流（5 分钟 5 次）、会话 `HttpOnly` + `SameSite=Lax`、JSON 请求体上限 4 MiB、请求头上限、120 s 读写超时、CSP 与 `X-Frame-Options: DENY`、`.Trash` 只经 API 访问。
- **同一份代码跨平台**：Windows（含 Windows 7）/ Linux（musl、armv7）/ Android，平台差异收敛在 `src/platform/` 里。

---

## 1. 实现概览

运行时与前端打包方式见上一节「亮点」，这里只列模块划分。

| 文件 | 职责 |
|---|---|
| `build.rs` + `src/inflate.rs` | 构建期把 `ui/*` 压成 gzip（定长 Huffman），运行时解压内嵌资源 |
| `src/main.rs` | 启动流程、日志初始化、监听与 MIME 覆盖装载 |
| `src/server.rs` | 监听/接入/连接处理、路由、静态资源、文件响应（Range/ETag/304/416） |
| `src/http.rs` | 请求解析、响应写出、`Expect: 100-continue` |
| `src/api.rs` | `/_api/*` JSON 接口：会话、登录、列表、新建、重命名、复制、删除、上传、文本读写、回收站 |
| `src/fsapi.rs` | 路径解析与越界防护、目录枚举、断点续传写入器 |
| `src/trash.rs` | 回收站：条目布局与 `meta.json`、移入/列表/恢复/清除/策略淘汰、旧扁平条目兼容 |
| `src/zip.rs` | 目录流式 ZIP 打包（存储式，`Content-Length` 预先可知） |
| `src/auth.rs` | 用户校验、会话 token、失败次数限流 |
| `src/config.rs` / `src/cli.rs` | `fern.conf` 与命令行解析、环境变量覆盖 |
| `src/json.rs` / `src/range.rs` / `src/mime.rs` / `src/util.rs` / `src/log.rs` | 基础库 |
| `src/platform/` | Windows（托盘/服务/防火墙/浏览器/控制台接管）、Unix（rlimit/chroot/setuid/syslog）、兜底 |
| `ui/` + `tools/make-icon.ps1` | 前端资源源文件与图标。脚本用 .NET 自带的 `System.Drawing` 画出 16/20/24/32/48/256 六个尺寸写进 `ui/fern.ico`，托盘代码 `include_bytes!` 内嵌该文件；脚本不参与 `cargo build`，改图标几何时需同步 `ui/tray-icon.svg`（设计稿）与脚本，再手动重跑 |

---

## 2. 构建

Rust 1.77.2 是最后一个支持 Windows 7 的稳定版；本项目按其要求使用 `*-pc-windows-gnu` 目标（无需 MSVC）。

```powershell
# Windows（需 MinGW 链接器，scoop 的 gcc 即可）
rustup install 1.77.2-x86_64-pc-windows-gnu
rustup default 1.77.2-x86_64-pc-windows-gnu
cargo build --release          # target\release\fern.exe

# Linux / Android（同一份代码）
cargo build --release --target x86_64-unknown-linux-musl
cargo build --release --target aarch64-linux-android
```

`.cargo/config.toml` 按目标三元组分别设置 rustflags（不能用全局 `[build] rustflags`，否则 `crt-static` 的语义会串到交叉目标上）；Windows 与 armv7 都带 `-C target-feature=+crt-static`，产物不依赖额外运行库。

### ARMv7（硬浮点 Linux 板卡）

已实测可用的交叉编译链路：主机 Windows + [Arm GNU Toolchain](https://developer.arm.com/downloads/-/arm-gnu-toolchain-downloads)（`arm-none-linux-gnueabihf-gcc`，13.3.rel1 验证通过）。

```powershell
# 1) 只需一次：装目标标准库
rustup target add armv7-unknown-linux-gnueabihf --toolchain 1.77.2-x86_64-pc-windows-gnu

# 2) 把工具链 bin 目录放进 PATH（没写进系统 PATH 时必须显式补）
$env:Path = "E:\arm-gnu-toolchain-13.3.rel1-mingw-w64-i686-arm-none-linux-gnueabihf\bin;$env:Path"

# 3) 构建
cargo build --release --target armv7-unknown-linux-gnueabihf   # target\armv7-unknown-linux-gnueabihf\release\fern
```

`.cargo/config.toml` 里已经把 `linker` 指向 `arm-none-linux-gnueabihf-gcc`，因此只要它在 `PATH` 上就能直接构建。若不想改 `PATH`，把该文件里的 `linker` 换成绝对路径（Windows 路径分隔符要转义成 `\\`）。

产物为 ELF 32 位、ARM EABI5、hard-float、**完全静态**（`readelf -d` 无 `.dynamic`，也没有 `INTERP`），约 **1.03 MiB**（1 082 212 字节），可直接拷到目标机运行。体积差异来自 glibc 静态库（`libc.a`）本身。

想要更小可以改成动态链接：在 `.cargo/config.toml` 的 `[target.armv7-unknown-linux-gnueabihf]` 里把 `rustflags` 换成 `-C target-feature=-crt-static`，产物降到约 **566 KiB**（579 440 字节），但要求目标机的 glibc 不低于构建时的版本（先在板上跑 `ldd --version` 对照）。

验证方式（无 qemu/WSL 时只能在实机跑）：

```sh
scp target/armv7-unknown-linux-gnueabihf/release/fern user@board:/tmp/
ssh user@board '/tmp/fern --check -c /tmp/fern.conf && /tmp/fern -w /tmp/share -l 4545'
```

若提示 `'cargo' 不是内部或外部命令`：`%USERPROFILE%\.cargo\bin` 通常已经在用户 `Path` 里（`rustup` 安装时写入），只是当前终端继承的是旧的环境快照。新开一个终端即可；不想重开时可临时补一句 `set PATH=%USERPROFILE%\.cargo\bin;%PATH%`（cmd）或 `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`（PowerShell）。

仓库不提供 `rust-toolchain.toml`：在 Windows 上 `channel = "1.77.2"` 会被解析为 MSVC 宿主工具链（本机没有 `link.exe`），因此请按上面的命令显式安装并使用 gnu 工具链，或在单目录执行 `rustup override set 1.77.2-x86_64-pc-windows-gnu`。

产物体积（x86_64-pc-windows-gnu，`opt-level="z"` + fat LTO + strip）：约 **598 KiB**（611 840 字节）。同样参数下「Hello world」的 Rust 静态链接二进制本身就有 253 KiB，因此 500 KiB 目标在当前技术选型下达不到——这是 Rust `std` + 静态 CRT 的下限。

体积上已经做过的两处取舍（都不会改变外部行为）：

- **前端资源构建期 gzip**：`build.rs` 用内置的定长 Huffman 压缩器（无第三方 crate）把 `ui/*` 压成 `OUT_DIR/*.gz`，`src/assets.rs` 用 `include_bytes!` 内嵌压缩后的数据，首次请求时由 `src/inflate.rs`（仅支持 stored / 定长 Huffman 块）解压到 `OnceLock` 缓存。55 KB 源码变成约 17 KB 常量，`ETag` 仍按内容计算，改动 UI 后自然失效。
- **JSON 数字用整数**：内部 JSON 只与本项目前端交换，`size` / `mtime` 均为整数，因此 `Json::Num` 用 `i64`，省掉了 `core` 的浮点格式化表（约 39 KiB）。API 不接受小数与科学计数法（返回 `400 invalid_json`）。

---

## 3. 运行

```powershell
.\fern.exe                                  # 默认：端口 4545，共享 exe 所在目录，访客 RL
.\fern.exe -w "D:\share" -l 4545            # 指定目录与端口
.\fern.exe -g 0 -u "admin:secret:RLUMST"    # 关闭访客权限，添加用户
.\fern.exe -c .\fern.conf --check           # 校验配置后退出
.\fern.exe -h -B                            # 隐藏托盘图标、不自动打开浏览器
```

| 选项 | 说明 |
|---|---|
| `-c <file>` | 配置文件；缺省时找 `./fern.conf`、`/etc/fern.conf` |
| `-w <dir>` | 共享根目录（缺省且没有配置文件时用 `fern.exe` 所在目录；有配置文件但未写 `Fern.Root` 时用 `./web`） |
| `-l <addr>` | 监听地址，可重复；`4545` / `1.2.3.4:4545` / `[::]:4545,Dual` |
| `-g <perms>` | 访客权限字母 `RLUMSTD`，`0` 表示禁用（禁用时页面直接显示登录表单，顶栏也不再出现「未登录 / 登录」） |
| `-u <spec>` | 添加用户：`user:pass:perms`（也接受 `perms:user:pass`） |
| `-h` | **隐藏托盘图标**（不是 help） |
| `-B` | 启动时不打开浏览器 |
| `--max-conns <n>` | 最大并发连接，默认 128 |
| `--log-dir <dir>` | 按天写 `fern-YYYY-MM-DD.log` |
| `--install-service` / `--uninstall-service` | Windows 服务注册（`sc.exe`） |
| `--add-firewall` / `--remove-firewall` | Windows 防火墙规则（`netsh`，规则名 `fern`） |
| `--check` | 校验配置并打印生效值 |
| `--help` / `--version` | 帮助 / 版本 |

不带 `-` 的裸参数会被当作配置文件路径，所以 `fern --check ./fern.conf` 与 `fern -c ./fern.conf --check` 等价（已同时给出 `-c` 时按冲突报错）。

权限字母：`R` 读取、`L` 列目录、`U` 上传、`M` 删除/重命名、`S` 显示隐藏项、`T` 网页播放、`D` 下载属性。输出与文档统一用大写；输入不区分大小写（`rl` 等价于 `RL`）。

环境变量：`_LOG_DIR` `_syslog` `_FD_LIMIT` `_RUNAS` `_CHROOT_PATH`（与原版一致），新增 `_MAX_CONNS`。

### 3.1 窗口与控制台行为（Windows）

`release` 构建按 **GUI 子系统**链接（`#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]`），所以双击 `fern.exe` 只有托盘图标，不会弹出黑色控制台窗口。`debug` 构建（`cargo build` / `cargo run`）仍是 console 子系统，方便开发时直接看输出。

GUI 子系统默认拿不到 stdout/stderr。为了在终端里依然可用，启动时执行 `AttachConsole(ATTACH_PARENT_PROCESS)` 附回父进程的控制台，只对空/无效的 `STD_OUTPUT_HANDLE`、`STD_ERROR_HANDLE` 用 `CONOUT$` 重新 `SetStdHandle`（已被重定向到管道或文件的句柄不动）。效果：

- 在终端里运行：`--help`、`--version`、`--check` 和日志照常打印到该终端。
- 从资源管理器双击：`AttachConsole` 失败即静默返回，什么都不弹。
- 致命错误（配置非法、端口被占用等）在没有控制台时改用 `MessageBoxW` 弹窗，不会「双击后毫无反应」。
- 程序中派生的外部命令（`cmd start` 打开浏览器、`netsh`、`sc.exe`）统一加 `CREATE_NO_WINDOW`，不会闪现黑框。

---

## 4. 配置

配置文件为 `fern.conf`：指令与取值大小写不敏感，`#` 起注释，含空格的路径加引号，未知指令报错退出（避免拼写错误静默生效）。

| 指令 | 说明 |
|---|---|
| `Fern.Root` / `Fern.Listen` / `Fern.User` / `Fern.Guest` | 共享目录、监听地址、用户与访客权限 |
| `Fern.Hide` / `Fern.Browser` | 隐藏托盘图标、指定托盘打开的浏览器 |
| `Fern.MaxConns` / `Fern.LogDir` | 并发上限、按天日志目录 |
| `Fern.Title` / `Fern.Notice` | 界面标题（默认 `fern`）与页头通知，见 §5.2 |
| `Fern.Mime <file>` | MIME 覆盖表 |
| `Trash.Enabled` / `Trash.Max.Days` / `Trash.Max.Size` / `Trash.Keep.Same` | 回收站保留策略（见 §5.1） |

MIME 覆盖文件每行 `<ext> <type>` 或 `<type> <ext> <ext>...`；工作目录与配置文件同目录下的 `fern.mime` 会自动加载，显式 `Fern.Mime` 声明的条目优先。示例见 [`fern.conf.example`](./fern.conf.example) 与 [`fern.mime.example`](./fern.mime.example)。

未指定 `-c` 时配置文件按**进程工作目录**查找 `./fern.conf`（Unix 上再找 `/etc/fern.conf`），不是按 exe 所在目录。所以在快捷方式、计划任务或别的目录里启动时，容易“配置文件没生效、权限全是默认值”。相对路径的 `Fern.Root` 同样相对工作目录解析。`--check` 会打印实际生效的端口、根目录与权限，可用来确认读到的是哪个配置。

**根目录的确定顺序**：`-w` > `Fern.Root` > 没找到配置文件时回退到 `fern.exe` 所在目录 > `./web`。也就是说，只要把一个 `fern.exe` 拷到任意目录双击，共享的就是这个目录本身（此时 `fern.exe` 与 `fern.conf` 也会出现在列表里）。

---

## 5. HTTP 接口

页面直链保持原版形式：`/<path>` 直接下载，目录返回 `302` 到 `/#<path>`；给目录加上 `?zip=1` 则改为打包下载该目录（见下表）。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/` | 内置前端 |
| GET | `/assets/{app.js,style.css,favicon.svg}` | 静态资源（内容哈希 ETag + `Cache-Control: no-cache`，靠 304 校验） |
| GET | `/files/<path>` | 文件下载，支持 `Range`、`If-None-Match`；带 `?download` 时 `Content-Disposition` 为 `attachment`，否则 `inline` |
| GET/HEAD | `/files/<dir>?zip=1` | 将目录流式打包为 ZIP（`Content-Type: application/zip`，`Content-Disposition: attachment`），`Content-Length` 在响应头即给出，全部条目按存储（不压缩）方式写入；符号链接跳过；总大小 > 3.6 GB 返回 `413 too_large`，条目数 > 65000 返回 `413 too_many_entries`；生成的归档无稳定 ETag 且不支持 `Range`（`Cache-Control: no-store`） |
| GET | `/_api/session` | 当前身份与权限，含 `trashEnabled`（决定前端是否显示「回收站」）、`title` / `notice`（界面标题与通知） |
| POST | `/_api/login` | `{"user","pass"}` → `Set-Cookie` |
| POST | `/_api/logout` | 清除会话 |
| GET | `/_api/list?path=/` | 目录列表 |
| POST | `/_api/mkdir` | `{"path","name"}` |
| POST | `/_api/newfile` | `{"path","name"}` → `{"ok":true,"path":"/dir/name","name"}`；在 `path` 目录下新建**空文件**，拒绝含 `/`、`\`、`..` 或内部后缀（`.fernpart`/`.fernedit`）的名字（`400 bad_name`），已存在则 `409 exists`，从不覆盖（需 `m`） |
| POST | `/_api/rename` | `{"from","to"}` |
| POST | `/_api/copy` | `{"paths":[…],"dest":"/dir"}` → `{"copied":N,"paths":[…]}`；递归复制文件/目录，重名自动改为 `<名字> - 副本`、`<名字> - 副本 (2)`，故可直接粘贴回源目录；符号链接跳过，禁止复制 `.Trash` 与把目录复制进自身（`400 copy_into_self`）（需 `m`） |
| POST | `/_api/delete` | `{"paths":[…]}` → 移入 `.Trash`；`Trash.Enabled 0` 时直接删除，响应带 `"permanent":true`（见 §5.1） |
| PUT/POST | `/_api/upload?path=&name=&offset=&total=` | 原始字节流，`offset` 实现断点续传；偏移不符返回 `409 {"error":"offset_mismatch","offset":N,"total":T}`；带 `restart=1` 才显式丢弃已收分片 |
| GET/HEAD | `/_api/upload?path=&name=` | 探测续传位置，返回 `{"offset":N,"exists":bool}`（`N` 为 `<name>.fernpart` 已有字节数） |
| DELETE | `/_api/upload?path=&name=` | 丢弃取消的上传残留（删除 `<name>.fernpart`） |
| GET/HEAD | `/_api/read?path=<path>` | 读取 UTF-8 文本供在线编辑，返回 `{"path","content","size","mtimeMs","bom","canEdit","maxBytes"}`；非文本 `415 not_text`，目录 `400 is_directory`，超过 2 MiB `413 too_large`（需 `r`） |
| PUT/POST | `/_api/write` | `{"path","content","mtimeMs","force","bom"}` 原子写回，保留 BOM 与文件权限；`mtimeMs` 与服务器不符返回 `409 {"error":"conflict","mtimeMs":N}`，`force:true` 可强制覆盖（需 `m`） |
| GET/HEAD | `/_api/trash?sort=&offset=&limit=` | 回收站列表，返回 `{"items":[…],"total","bytes","enabled","maxDays","maxBytes","keepSame"}`；每项含 `id,name,path,isDir,size,files,partial,deletedAt,deletedBy,via,legacy`（需 `m`） |
| POST | `/_api/trash/restore` | `{"ids":[…],"overwrite":false}` → `{"restored":N,"paths":[…],"failed":[…]}`；目标已存在且未 `overwrite` 时返回 `409 {"code":"exists",…}`，开启后原文件反向入站（需 `m`） |
| POST | `/_api/trash/purge` | `{"ids":[…]}` → `{"purged":N}`，永久删除指定条目（需 `m`） |
| POST | `/_api/trash/empty` | `{}` 或 `{"olderThanDays":N}` → `{"purged":N}`，清空回收站（需 `m`） |

HTTP/1.1 默认保持连接（`Connection: keep-alive`，上限 100 请求/连接），`Connection: close` 或 HTTP/1.0 则一次性关闭；带请求体的 `POST`/`PUT` 处理完即关闭，避免残留字节污染下一个请求。

上传分片先写入 `<name>.fernpart`，收满 `total` 后同步并原子重命名为目标文件。`.fernpart` / `.fernedit` 属于服务端内部临时文件，目录列表一律按隐藏项处理（需 `S` 权限才可见）。取消上传会删除残留分片，因此断点续传只在「同一目录、同名文件」下生效。前端上传行底部的提示实时显示 `已传输 / 总大小` 与本会话平均速度，命中残留分片时额外标注「断点续传」，无需用户点击任何按钮。

上传文件夹时 `name` 允许带 `/`（源自 `webkitRelativePath`，如 `GVMaker/data/a.dat`），目标子目录即使尚不存在也会自动创建，无需先手动新建。任何提前返回的错误（`403 forbidden`、`400 bad_name`、`409 is_directory` 等）都会先把请求体读完再回包，否则关闭仍带未读字节的连接会触发 TCP RST，浏览器只能报 `Failed to fetch` 而看不到 JSON 内容；前端据此把错误码翻译成中文提示（如「同名文件夹已存在」）。

下载目录必须带 `?zip=1`：目录直链本身仍是 `302` 跳转到前端，浏览器会把跳转后的 HTML 页面当成文件保存下来（表现为下载到一个 `<目录名>.htm`）。前端选中目录时自动请求 `/files/<dir>?zip=1` 并追加 `.zip` 后缀作为保存文件名，选中普通文件则保持原链接与原名。

内置前端资源始终 `Cache-Control: no-cache`，ETag 由资源内容编译期哈希得出（`<文件名>-<16 位十六进制>`），内容一变 ETag 必变，升级后浏览器不会继续沿用旧 UI。若升级后看到旧界面，说明浏览器仍持有旧版本发出的长缓存副本，强制刷新（`Ctrl+F5`）一次即可。

选中文件或目录后工具栏出现「复制」按钮（快捷键 `Ctrl+C`），点「粘贴副本」即把副本放到当前目录，粘贴成功后剪贴内容立即清空、按钮随之隐藏，需要再次点「复制」；「剪切」仍走 `Ctrl+X` →「粘贴」的移动流程。复制粘贴直接落在同一目录时，副本命名为 `<名字> - 副本`（文件保留扩展名，如 `conf - 副本.toml`），同名已存在则递增为 `<名字> - 副本 (2)`，因此反复复制粘贴不会互相覆盖。

工具栏的「新建文件夹」与「新建文件」都需要 `m` 权限，在回收站视图中隐藏。「新建文件」通过 `POST /_api/newfile` 直接落一个空文件（已存在时 `409`，绝不覆盖），创建成功后自动打开在线编辑器，可以直接输入内容再按 `Ctrl+S` 保存，省去「新建 → 上传 → 覆盖」的往返。

### 5.1 回收站

删除的文件不会真正消失，而是移入 `<root>/.Trash/`，前端通过工具栏的「回收站」按钮进入（路由 `#/__trash`，不用 `/.trash` 以免与 Windows 大小写不敏感的 `.Trash` 目录冲突）。

磁盘布局（每条删除记录一个目录，便于原子提交与按条恢复）：

```
<root>/.Trash/<id>/
  meta.json      # id,name,origPath,isDir,size,files,partial,deletedAt,deletedBy,via
  data/<name>    # 原始文件，或整个目录树（保留内部结构）
```

`<id>` 由删除时刻的 base36 秒级时间戳生成，同秒冲突时追加 `-N` 后缀。移动过程先写入 `.Trash/.tmp-<id>` 再原子重命名，中途断电不会留下半截条目；启动时会顺带清理超过 1 小时仍未完成的 `.tmp-*` 残留。

`meta.json` 丢失或损坏的条目仍会出现在列表中（标记 `legacy:true` / `partial:true`），可直接永久删除，但无法恢复原名。原版留下的扁平条目 `.<base36>.<name>` 也可列出、恢复与清除，且始终保持只读，不会被改写成新布局。

`.Trash` 目录本身不对普通浏览开放：`GET /_api/list?path=/.Trash` 返回 `403`，`GET /files/.Trash/...` 返回 `404`，只能经 `/_api/trash*` 访问（需 `m` 权限）。

保留策略通过 `fern.conf` 或环境变量配置：

| 指令 | 环境变量 | 默认值 | 说明 |
|---|---|---|---|
| `Trash.Enabled` | `_TRASH_ENABLED` | `1` | `0` 时删除直接落地（等同原版行为），前端隐藏「回收站」按钮，删除确认改为「永久删除…无法撤销」提示 |
| `Trash.Max.Days` | `_TRASH_MAX_DAYS` | `30` | 保留天数，`0` 表示不按时间清理 |
| `Trash.Max.Size` | `_TRASH_MAX_SIZE` | `1024`（MiB） | 回收站总容量上限，`0` 表示不限制 |
| `Trash.Keep.Same` | `_TRASH_KEEP_SAME` | `10` | 同名文件最多保留几份（另存为历史版本） |

清理在启动时以及每次批量删除后触发，顺序为：先按天数淘汰，再按同名份数淘汰，最后按总容量从最旧开始淘汰。`.fernpart` / `.fernedit` 这类服务端临时文件按内部文件直接丢弃，不占用回收站。

### 5.2 界面标题与通知

| 指令 | 默认值 | 说明 |
|---|---|---|
| `Fern.Title` | `fern` | 浏览器标签页标题与页头左上角品牌文字 |
| `Fern.Notice` | 空（不显示） | 有值时在页头品牌右侧显示一个通知胶囊，过长自动省略（悬停看全文） |

两者都通过 `GET /_api/session` 下发（`title` / `notice` 字段），前端用 `textContent` 写入，因此 HTML 字符不会被当作标记解析。值含空格时建议加引号；含 `#` 时**必须**加引号（否则 `#` 之后会被当作注释）。超过 200 字符会报错退出，避免把页头撑坏。示例：

```
Fern.Title "团队 网盘"
Fern.Notice "每周日 02:00-03:00 停机维护"
```

`--check` 会同时打印当前的 `title` 与 `notice`，便于确认配置已生效。

---

## 6. 测试与验证状态

```powershell
cargo test     # 46 个单元测试 + 25 个端到端集成测试
```

集成测试（`tests/integration.rs`）会真实启动编译出的二进制，覆盖：静态资源与 304、访客权限、Range/416/条件下载、目录 302、路径穿越、登录与文件管理（mkdir/rename/delete → `.Trash`）、复制（目录递归、同名递增、跨目录、权限与错误码）、新建空文件（重名 `409`、非法名称 `400`、权限）、回收站列表/恢复/冲突覆盖/清除与权限、分块续传与 409 偏移纠正、标题与通知配置（默认值与 `Fern.Title` / `Fern.Notice` 覆盖）、方法与路由守卫、Keep-alive 连接复用与关闭语义。

已验证：

- Windows x86_64 GNU 构建通过，`--check` 解析 `fern.conf` 成功。
- 实测接口：会话、登录/登出、列表、新建目录/新建空文件、重命名、复制（文件/目录/同目录多副本）、删除、回收站（列表/恢复/冲突覆盖/彻底删除/清空）、分块续传、206/304/416、405、目录 302、穿越拦截。
- 浏览器实测首页渲染正常（顶栏权限、面包屑、工具栏、列表、状态栏）；`#/__trash` 回收站页面渲染、选中、恢复、彻底删除、清空与「返回文件」跳转均通过。
- 浏览器实测工具栏吸顶：60 个文件的目录滚动 1400 px 后工具栏 `getBoundingClientRect().top` 恒为 `0`、`elementFromPoint` 命中的是工具栏而不是列表行，此时直接点「新建文件夹」按钮能触发；滚回顶部阴影消失。400 px 窄屏下工具栏换行成两行仍吸顶、无横向溢出、按钮可点。
- 浏览器实测「复制」→「粘贴副本」在源目录生成 `hello - 副本.txt`，再次粘贴得到 `hello - 副本 (2).txt`；`Ctrl+X` → 切换目录 →「粘贴」的移动流程未受影响。
- 托盘图标创建成功（日志 `tray icon added`），通知区域显示的是内嵌的 fern 图标而非系统通用图标；`ui/fern.ico` 的六个尺寸都能经 `CreateIconFromResourceEx` 还原成 `HICON`（16/24/32/48/256 实测逐一验证）。托盘菜单 `Exit` 会先移除图标再结束进程（日志 `exit requested from the tray menu`，退出码 0、端口立即释放），不会留下无图标的服务器继续监听。
- 无控制台行为：`release` 产物 PE 头 `Subsystem = 2`（GUI）；用 `Win32_Process.Create` 模拟资源管理器启动（父进程无控制台）后枚举该进程顶层窗口，只有隐藏的 `fern_tray_window` 与系统 IME 窗口，**没有** `ConsoleWindowClass`，即双击不弹黑框。从真实控制台运行 `--version` / `--check` 时输出正常落在该控制台（截图核对 `fern 0.1.0` 与六项生效值）；`Start-Process -RedirectStandardOutput` 也能拿到 `fern 0.1.0`，说明重定向句柄没有被 `SetStdHandle` 破坏。托盘「打开浏览器」走了 `CREATE_NO_WINDOW` 的 `cmd start`：触发前后桌面控制台窗口数不变，且日志里随即出现 `_api/session`、`_api/list` 请求，浏览器正常打开。
- 浏览器实测 `Fern.Title` / `Fern.Notice`：标签页标题与顶栏品牌文字随配置变化，通知为空时不占位。
- 浏览器实测图标工具栏：15 个按钮全部为内联 SVG（`stroke: currentColor`，18 px，随深/浅色自动变色），逐个放大截图核对图形正确（新建文件夹为加粗外框的文件夹 + 加号，文件夹外框收窄加高至接近正方形、不再是扁长方形，上传文件夹为与「新建文件夹」一致的外框内嵌一支向上箭头（箭头完整收在文件夹轮廓内，不越出边框），回收站为三箭头循环三角且箭头与下一段箭杆之间留有明显缺口（放大后不粘连）、进入回收站后换成回旋返回箭头）；悬停 `::after` 气泡 `opacity` 由 `0` 变为 `1`、`content` 为对应 `data-tip`，气泡向下弹出不遮挡吸顶条；400 px 窄屏下工具栏 `scrollWidth === clientWidth`、页面 `documentElement.scrollWidth === clientWidth`，无横向溢出。状态类按钮随上下文切换：选中文件后出现「复制/重命名/删除/下载」，复制后「粘贴」变为「粘贴副本」（同时 `data-tip` 与 `aria-label` 更新），进入回收站后图标切为返回箭头、提示变为「返回文件」，回退后复原。
- 内嵌资源 gzip 往返验证：单元测试比对 `include_bytes!(ui/*)` 与解压结果一致，集成测试 `serves_ui_and_assets` 校验响应体与源码逐字节相同；实测 `/`、`/assets/app.js`、`/assets/style.css`、`/assets/favicon.svg` 的 `Content-Length` 分别为 6379 / 40317 / 13000 / 873，`If-None-Match` 复验返回 `304`。
- armv7 交叉编译通过（Arm GNU Toolchain 13.3.rel1 + `armv7-unknown-linux-gnueabihf`）：1 082 212 字节，ELF32 ARM / EABI5 / hard-float，无 `.dynamic`、无 `INTERP`，`.symtab` 已 strip；在嵌入式设备上实机启动并输出 `--help` 正常。`x86_64-pc-windows-gnu` 宿主产物 640 000 字节（含 23.5 KB 图标），`cargo test` 46 + 25 全绿。

未验证 / 未实现：

- armv7 产物未在实机运行（本机无 `qemu-arm`，也未安装 WSL 发行版）；`aarch64-linux-android` / `x86_64-unknown-linux-musl` 目标未安装未构建。
- Unix 的 `_RUNAS` / `_CHROOT_PATH` / `_syslog`、Windows 服务与防火墙规则未在真实环境跑通。
- 守护进程化（fork/setsid）未实现，请用 systemd / procd 等外部工具托管。
- Windows 7 实机运行未验证（工具链与静态 CRT 已按其要求选择）。

---

## 7. 许可证

[Apache License 2.0](LICENSE) · Copyright 2026 JsBeta

- 本仓库代码（`src/`、`build.rs`、`tests/`、`tools/`）与内嵌前端资源（`ui/`）均由本项目自行实现，无第三方 crate 依赖；分发或修改时请保留 `LICENSE`，如有 `NOTICE` 则一并保留。
- 二进制产物以静态方式链接 C 运行库，需注意对应运行库的许可：`x86_64-unknown-linux-musl` 链接 musl（MIT），静态链接无额外义务；本文档给出的 armv7 链路（`arm-none-linux-gnueabihf-gcc` + `-C target-feature=+crt-static`）静态链接的是 glibc `libc.a`，glibc 属 LGPL-2.1-or-later，分发二进制时需一并提供可重新链接的手段（对应的目标文件，或源码 + 构建脚本）；Windows 目标的 `crt-static` 指向 MSVC/MinGW 运行库，建议在发布页注明工具链版本。
