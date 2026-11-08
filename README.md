# ld-java-analyze

面向 **Java 8 / Maven** 的代码导航插件：VS Code 薄扩展 + Rust LSP server。
自建 `Symbol` / `Occurrence` / `Link` 索引，支持定义、引用（可按目录过滤）、实现、
MyBatis XML ↔ 接口跳转、Lombok 合成符号、库方法源码优先与多 JAR 候选。

维护与迭代规范见 [`AGENT.md`](AGENT.md)；本文只讲**从源码到可用插件**的流程与日常使用。

---

## 快速开始：从构建 `ld-java-analyze.exe` 到安装使用

前置条件：Rust（≥1.85）、Node.js（≥18）、VS Code CLI `code`、Maven（用于生成 classpath）。

下文有两个目录，别混淆：

| 目录 | 用途 |
| --- | --- |
| `e:\projects\vsjavapro\demo1\ld_java_analyze` | **插件源码**（本节所有构建命令在这里执行） |
| `e:\projects\vsjavapro\demo1` | **被索引的业务项目**（VS Code 要打开的工作区，状态目录 `.ld-java-analyze/` 在这里） |

**一条最小回路**（PowerShell，顺序不可颠倒）：

```powershell
# ① 构建并验证 LSP server —— 在插件源码目录
cd e:\projects\vsjavapro\demo1\ld_java_analyze
cargo build --workspace
cargo test  --workspace
node testdata/verify-lsp.mjs        # 端到端验证，全部 PASS 才可继续

# ② 调整 VS Code 插件：把新 server 放进扩展目录并编译
Copy-Item target\debug\ld-java-analyze.exe editors\vscode\server\ld-java-analyze.exe -Force
cd editors\vscode
npm install                         # 幂等；CI 干净环境用 npm ci
npm run typecheck

# ③ 打包并安装到本机 VS Code
npm run vsix                        # 先 compile，再生成 ld-java-analyze-0.1.0.vsix
npm run install:local               # code --install-extension ... --force

# ④ 确认安装；然后到 VS Code 里 Reload Window
code --list-extensions --show-versions | Select-String ld-java-analyze
```

`npm run vsix` 已包含编译，所以只改 `src/extension.ts` 或 `package.json` 时重跑 ②–④；
改动 Rust 代码才需要重做 ① 的构建与 `Copy-Item`。完整闸门见步骤 1。

### 步骤 1 — 构建并验证 `ld-java-analyze.exe`

```powershell
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node testdata/verify-lsp.mjs
```

`verify-lsp.mjs` 用真实 stdio JSON-RPC 会话覆盖定义、引用、目录过滤、MyBatis 双向跳转、
Lombok、实现、高亮、悬停、符号与扫描规则。全部 PASS 且退出码 0 才算通过。
产物是 `target/debug/ld-java-analyze(.exe)`；用 `LD_JAVA_ANALYZE_SERVER` 环境变量可以让该脚本验证
已安装扩展里的 server。

### 步骤 2 — 调整 VS Code 插件

扩展源码在 `editors/vscode/`：`src/extension.ts`（命令与 UI）、`package.json`
（配置项、命令、激活事件）、`README.md`（扩展市场页）。

```powershell
cd editors\vscode
npm install            # 安装/同步依赖，幂等（CI 在干净 checkout 上用 npm ci）
npm run typecheck      # 只检查类型
npm run compile        # 生成 out/extension.js
```

扩展启动时会优先使用自带的 `server/ld-java-analyze(.exe)`，回退到开发目录
`../../target/debug/ld-java-analyze(.exe)`。**修改 Rust 后必须重新复制二进制**到
`editors/vscode/server/`，否则 vsix 里装的还是旧 server。该目录被 gitignore，
不会进版本库。

开发调试也可以不打包：VS Code 打开 `ld_java_analyze` 目录，用「Run Extension」
启动扩展开发宿主；此时自动回退到 `target/debug` 的二进制。

### 步骤 3 — 打包并安装

```powershell
cd editors\vscode
npm run vsix            # = npm run compile && vsce package --allow-missing-repository --out ld-java-analyze-0.1.0.vsix
npm run install:local   # = code --install-extension ld-java-analyze-0.1.0.vsix --force
```

VSIX 内含 `out/extension.js`、`server/ld-java-analyze(.exe)`、`LICENSE.txt`、`readme.md`，
安装后不需要 Rust 工具链。
安装结果：`code --list-extensions --show-versions` 显示 `local.ld-java-analyze@0.1.0`，
目录为 `%USERPROFILE%\.vscode\extensions\local.ld-java-analyze-0.1.0`。

### 步骤 4 — 重载并使用

1. **`Developer: Reload Window`** —— 已运行的窗口必须重载才会加载新 `extension.js`。
2. 把**被索引的项目根**作为工作区打开（例如 `E:\projects\vsjavapro\demo1`），
   不要打开 `ld_java_analyze`；插件只读 `<工作区根>/.ld-java-analyze/`。
3. 打开任意 `.java` / `.xml` 文件即激活。命令面板执行
   `ld-java-analyze: 自检所有命令`，应全部 OK；再执行 `ld-java-analyze: 查看索引状态`
   确认 `root`、`files`、`compiledJars` 符合预期。
4. 所有命令结果同时写入 **「ld-java-analyze」输出面板**。日志在
   `<工作区根>/.ld-java-analyze/logs/`（需先启用，见下文）。
5. 想让库方法跳源码，在业务项目根运行「配置被索引的项目」里的
   `scripts/setup-classpath.ps1`（或 `.sh`），再执行 `ld-java-analyze: 重建索引`
   （文件监听只覆盖 `*.java` / `*.xml`，`.txt` 变化不会自动重读）。

> 没有 classpath 时，项目内源码的定义/引用/高亮/MyBatis/Lombok 仍可用。

---

## 配置被索引的项目

插件不启动 `mvn`、不联网、不下载源码，只读以下用户生成的文件。所有状态位于
`<VS Code 工作区根>/.ld-java-analyze/`：

```
<workspace>/.ld-java-analyze/
├── classpath.txt    用户用 Maven 生成的只读 classpath
├── ignore           可选的扫描排除规则（gitignore 风格）
├── config.json      可选的日志配置
├── cache/           索引快照、从 sources jar 解出的 .java、无源码时的结构视图
└── logs/            启用 logDir 后的按天日志
```

### classpath 与源码 jar

在**被索引项目根**运行仓库自带的脚本即可。当前目录自动作为工作区根，
不需要手写绝对路径，也不需要猜哪个模块依赖最全。

Windows（PowerShell）：

```powershell
cd E:\projects\vsjavapro\demo1
& E:\projects\vsjavapro\demo1\ld_java_analyze\scripts\setup-classpath.ps1
```

若提示执行策略限制，改用
`powershell -NoProfile -ExecutionPolicy Bypass -File E:\projects\vsjavapro\demo1\ld_java_analyze\scripts\setup-classpath.ps1`。

Linux / macOS / CI：

```bash
cd /path/to/project
bash /path/to/ld_java_analyze/scripts/setup-classpath.sh
```

想先看会执行什么，加 `-DryRun` / `--dry-run`：不跑 Maven、不写任何文件。

脚本自动完成：

1. 创建 `<当前目录>/.ld-java-analyze/`。
2. `mvn -q -U install -DskipTests` —— 把 reactor 内部 `*-SNAPSHOT` 模块装进本地仓库；
   `-U` 清掉「上次解析失败」的缓存标记。
3. `mvn -q dependency:sources` —— 下载 `*-sources.jar`。
4. 读取根 `pom.xml` 的 `<modules>`，逐个以 `-pl <module>` + **绝对路径**的临时文件探测，
   取 jar 数最多的一份写入 `.ld-java-analyze/classpath.txt`；单模块项目跳过探测直接生成。
   临时文件都放在状态目录内，不会散落到子模块。

常用参数：

| PowerShell | bash | 说明 |
| --- | --- | --- |
| `-Module demo1-web` | `--module demo1-web` | 直接指定最终模块，跳过自动探测（最快） |
| `-DryRun` | `--dry-run` | 只打印将执行的命令，不跑 Maven、不写文件 |
| `-SkipInstall` | `--skip-install` | 内部 SNAPSHOT 已安装时跳过 `mvn install` |
| `-SkipSources` | `--skip-sources` | 跳过 sources jar 下载 |

结束时会打印 jar 数、实际存在的 jar 数与 sources jar 数。之后在 VS Code 执行
`ld-java-analyze: 重建索引`（或 Reload Window），再用 `ld-java-analyze: 查看索引状态`
确认 `compiledJars` / `sourceJars`。没有源码 jar 时降级为**字节码结构视图**文本，不是反编译。

> 插件本身不启动 `mvn`、不联网、不下载源码；上述动作完全由脚本在你的项目里执行。
> 不要在 reactor 根直接跑 `mvn dependency:build-classpath`：该 goal 对每个模块各执行一次，
> 相对 `outputFile` 会按各模块 basedir 解析而散落多份，根目录那份只来自 `packaging=pom`
> 的父 pom。脚本用 `-pl` + 绝对路径规避。单模块的手写等价命令，以及 PowerShell 下
> `-D` 参数必须加引号（否则报 `Unknown lifecycle phase ".outputFile=..."`）的说明，
> 见 `testdata/demo1/classpath.example.txt`。

### 扫描排除规则

按**目录名 / 文件名**匹配（不写路径），三种入口叠加：内置目录排除
（`ld-java-analyze.useBuiltinExcludes`，默认开）、VS Code 设置 `ld-java-analyze.excludeRules`、
`<workspace>/.ld-java-analyze/ignore` 文件。

| 写法 | 含义 |
| --- | --- |
| `# 注释` / 空行 | 忽略 |
| `generated/` 或 `dir:generated` | 排除该**目录名** |
| `dir:build*` | 目录名通配 |
| `file:*.generated.java` | 排除匹配的**文件名** |
| `*Test.java` | 目录名和文件名都匹配 |
| `!file:KeepTest.java` | 反选；**最后一条匹配的规则生效** |

`*` / `?` 通配、大小写不敏感。`.ld-java-analyze` 始终排除且不能反选。
完整示例见 `testdata/demo1/ignore.example.txt`；用 `ld-java-analyze: 编辑扫描排除规则`
创建文件，保存后自动重建索引。

### 日志

默认关闭。启用方式（任选其一）：VS Code 设置 `ld-java-analyze.logDir`、
命令 `ld-java-analyze: 设置日志输出目录`、或 `<workspace>/.ld-java-analyze/config.json`：

```json
{ "logDir": ".ld-java-analyze/logs" }
```

相对路径按工作区根解析。按 UTC 日期分文件 `ld-java-analyze-YYYY-MM-DD.log`，
**最多保留 7 天**，LSP 每次启动自动清理，只删这一命名模式的文件。
日志只写文件，绝不写 stdout（会破坏 stdio JSON-RPC）。
记录启动参数、索引统计、解析失败、定义跳转决策链、引用查询、命令执行与关闭。
`ld-java-analyze.stats` 返回当前 `logDir` / `retentionDays`。

### 配置速查

配置键前缀为 `ld-java-analyze.*`；工作区状态目录为
`<工作区根>/.ld-java-analyze/`（存放 classpath、config、ignore 与缓存）。

| 设置 | 默认 | 说明 |
| --- | --- | --- |
| `ld-java-analyze.serverPath` | `""` | LSP 二进制绝对路径；留空用扩展自带 server |
| `ld-java-analyze.referenceRoots` | `[]` | 引用过滤目录；空 = 整个工作区 |
| `ld-java-analyze.excludeRules` | `[]` | 扫描排除规则 |
| `ld-java-analyze.useBuiltinExcludes` | `true` | 内置目录排除开关 |
| `ld-java-analyze.logDir` | `""` | 日志目录；空 = 不写日志 |

---

## 能力与命令

| 能力 | LSP 方法 |
| --- | --- |
| 跳定义（含 Lombok / record 合成符号）、库方法源码优先 | `textDocument/definition` |
| 查引用（可按目录过滤）、同文件读写高亮 | `textDocument/references` / `documentHighlight` |
| 文件结构、工作区符号、悬停 | `documentSymbol` / `workspace/symbol` / `hover` |
| 跳类型定义、跳实现（Spring bean 注入复用） | `typeDefinition` / `implementation` |
| MyBatis XML ↔ Mapper 方法双向跳转 | `textDocument/definition`（`Link`） |

自定义命令（`workspace/executeCommand`）：

| 命令 | 说明 |
| --- | --- |
| `ld-java-analyze.reindex` | 增量重建索引并写回快照 |
| `ld-java-analyze.setReferenceRoots` / `ld-java-analyze.referencesInPaths` | 设置过滤目录 / 按目录查引用 |
| `ld-java-analyze.jarCandidates` | 列出同名类的全部 JAR 候选（含源码与字节码） |
| `ld-java-analyze.setExcludeRules` / `ld-java-analyze.getExcludeRules` | 读写扫描排除规则 |
| `ld-java-analyze.setLogDir` | 运行时切换或关闭日志 |
| `ld-java-analyze.stats` | 索引统计、生效规则、`logDir` / `retentionDays` |

命令面板对应：`在指定目录下查找引用`、`设置引用过滤目录`、`列出同名类所在的 JAR`、
`编辑扫描排除规则`、`设置日志输出目录`、`重建索引`、`查看索引状态`、`自检所有命令`。

交互约定：目录选择与结果展示用 **QuickPick**，所有结果写入
**「ld-java-analyze」输出面板**；取消、空结果、服务未就绪、请求失败都有明确提示。
`自检所有命令` 一次验证服务端各命令链路，非 Java/XML 文档会标记 SKIP。

---

## 验收与排障

```powershell
node testdata/verify-lsp.mjs
# 验证已安装扩展里的 server
$env:LD_JAVA_ANALYZE_SERVER = "$env:USERPROFILE\.vscode\extensions\local.ld-java-analyze-0.1.0\server\ld-java-analyze.exe"
node testdata/verify-lsp.mjs
```

`testdata/demo1` 是验收工程，覆盖跨文件调用、引用过滤、实现跳转、Lombok、
MyBatis 双向 Link、类型定义与扫描规则（含 `generated/Noise.java`、
`Temp.generated.java`、`other/ExternalCaller.java` 等边界样例）。

排障顺序：

1. 命令点击无反应 → `Developer: Reload Window`；再跑 `ld-java-analyze: 自检所有命令`。
2. 索引文件数异常 → `ld-java-analyze: 查看索引状态`，检查 `excludeRules` 与工作区根。
3. 库方法跳不到源码 → 确认 `<工作区根>/.ld-java-analyze/classpath.txt` 是最终模块生成的完整版本，
   且同目录存在 `*-sources.jar`；改完执行 `ld-java-analyze: 重建索引`。
4. 仍无头绪 → 启用 `logDir`，查看 `.ld-java-analyze/logs/` 中定义跳转的决策链与命令完成记录。

---

## 架构与限制

```
nav-core (模型/规则) → nav-java (解析/Lombok/MyBatis) → nav-index (唯一索引真相源)
                     ↘ nav-maven / nav-jar (只读发现 / JAR 候选) ↗
                                  → ld-java-analyze (LSP/日志) → editors/vscode (薄客户端)
```

索引是唯一真相源，框架只返回 `Link`；`WorkspaceIndex` 快照存于 `.ld-java-analyze/cache/`，
解析器输出结构变化时 `INDEX_VERSION` 递增使旧快照失效。

已知限制：跨文件绑定是启发式（名称 + 类别 + 接收者类型 + 实参个数），未做泛型实参推导、
参数类型级重载匹配与完整作用域分析；Java 语法以 Java 8 为主并最小支持 record，
其它 Java 16+ 特性未覆盖；无源码时是结构视图而非反编译；索引快照用 JSON 而非 `redb`。

设计取舍、命令注册约束、提示信息规范与已知陷阱统一写在 [`AGENT.md`](AGENT.md)。
CI 闸门见 `.github/workflows/ci.yml`（Rust fmt/clippy/test + TS typecheck + VSIX 打包）。
