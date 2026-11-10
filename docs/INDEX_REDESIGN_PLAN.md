# Java 轻量导航索引重构方案

> 目标：为 5 万级 Java 文件、90% 文件透明加密、VS Code 编辑器内只做“跳转”的场景，设计一套可增量维护、可冷启动、读写不互相阻塞的本地导航索引。
>
> 本方案是“从零设计”的目标架构，不要求沿用现有 `WorkspaceIndex` 实现；落地时可以新建内核，再逐步替换现有外壳。

## 当前进度

- **M0：架构契约** 已完成。
  - 新增 `crates/nav-next`
  - 定义 `FileId` / `SymbolId` / `FileContribution` / `IndexSnapshot` / `QueryEngine`
  - 支持接口↔实现、传递继承、方法搜索、名字绑定

- **M1：内容与文档模型** 已完成。
  - `DocumentStore`：常驻打开文档文本、版本、明文 hash
  - `ContentSource` / `StaticContentSource`
  - `WorkspaceFiles`：路径与 `FileId` 双向映射
  - `FileChangeDetector`：以明文 hash 为权威的变更判断
  - 使用 `blake3` 计算明文指纹

- **M2：不可变快照与查询** 已完成。
  - `SnapshotStore`：`ArcSwap<IndexSnapshot>` 原子发布
  - `symbol_at`：优先选择最小范围的 Definition occurrence
  - `definition_at` / `implementations_at` / `supertypes_at`
  - 旧 `QueryEngine` 在新快照发布后仍持有旧快照

- **M3：redb 持久化** 已完成。
  - 使用 `redb 4.5.0`
  - `StoredFile`：`path + FileContribution + FileFingerprint`
  - `postcard` 序列化、按 `FileId` 存储
  - 支持保存、删除、重启加载
  - 支持从持久化记录重建 `IndexSnapshot`

- **独立 Java 解析器** 已完成。
  - `nav-next::JavaAnalyzer`
  - 直接使用 `tree-sitter-java`
  - 支持 class/interface/record/enum、方法、字段、构造器
  - 支持 `extends` / `implements` 的 unresolved type edge
  - 支持 UTF-16 位置
  - 不复用旧 `nav-java`

- **独立 LSP Server** 已完成。
  - 新增 `crates/ld-java-next`
  - 使用 `tower-lsp-server`
  - 支持定义、实现、引用、文档符号、工作区符号
  - 支持方法全路径搜索、统计、全量重建、目录刷新
  - 支持编辑 debounce
  - 支持自定义 `ld-java-next/readTextDocuments` 明文桥
  - 支持指定目录强制刷新
  - 独立于旧 `ld-java-analyze`

- **独立 VS Code 扩展** 已完成。
  - 新增 `editors/vscode-next`
  - 使用 `vscode-languageclient`
  - 注册方法搜索、统计、重建、目录刷新命令
  - 实现 VS Code 明文读取桥
  - 已打包并安装为 `local.ld-java-next@0.1.0`

- **P0：核心跳转补齐** 已完成。
  - 局部变量符号化
    - `local_variable_declaration`
    - `catch_formal_parameter`
  - 参数符号化
    - `formal_parameter`
  - 方法调用按接收者类型绑定
    - `registerService.register(...)`
    - `User.create(...)`
  - 静态方法接收者和方法名跳转
  - 编辑 debounce
  - 单文件 `replace_file` 更新
  - 回归测试已覆盖：
    - 局部变量
    - 参数
    - 接收者类型
    - 静态方法
    - 字段声明

---

## 任务索引

后续开发直接按下面入口定位：

| 优先级 | 状态 | 任务 | 主要代码入口 |
|---|---|---|---|
| P0 | 已完成 | 局部变量 / 参数符号 | `crates/nav-next/src/java.rs` |
| P0 | 已完成 | 方法调用接收者类型绑定 | `crates/nav-next/src/java.rs` |
| P0 | 已完成 | 静态方法跳转 | `crates/nav-next/src/java.rs` |
| P0 | 已完成 | 编辑 debounce | `crates/ld-java-next/src/backend.rs` |
| P1 | 待做 | 完整单文件 delta 与局部重绑 | `crates/nav-next/src/index.rs` |
| P1 | 待做 | MyBatis XML ↔ Java Mapper | 新增 `crates/nav-next/src/mybatis.rs` |
| P1 | 待做 | Lombok 合成符号 | `crates/nav-next/src/java.rs` |
| P1 | 待做 | 外部依赖 / JAR / classpath 索引 | 新增 `crates/nav-next/src/dependency.rs` |
| P1 | 待做 | `notify` 文件监听 | `crates/ld-java-next/src/backend.rs` |
| P1 | 待做 | `zstd` 压缩与分片存储 | `crates/nav-next/src/store.rs` |
| P1 | 待做 | 性能基线与统计 | `crates/ld-java-next/src/backend.rs` |
| P2 | 待做 | 更完整 Java 语义绑定 | `crates/nav-next/src/java.rs` |
| P2 | 待做 | 引用结果分组与 UI 优化 | `editors/vscode-next/src/extension.ts` |
| P2 | 待做 | CI / 自动打包 / 索引恢复 | 根目录与 `editors/vscode-next` |

---

## 1. 目标

### 1.1 功能目标

- 支持以下导航能力：
  - `definition`
  - `typeDefinition`
  - `implementation`
  - `references`
  - `documentHighlight`
  - `workspaceSymbol`
  - 方法全路径搜索
  - MyBatis XML ↔ Java Mapper 方法跳转
- 支持单文件编辑后的秒级索引更新。
- 支持指定目录的强制重建，作为 watcher 失效或索引不一致时的兜底。
- 支持冷启动时复用已有索引，不做全量扫描。
- 支持透明加密文件：
  - 磁盘只做 `stat`
  - 内容通过 VS Code 桥获取明文
  - 明文只驻留内存，不落盘

### 1.2 非目标

- 不做完整 Java 编译器语义。
- 不做泛型推导、完整重载解析、完整类型检查。
- 不做代码补全、签名提示、重构。
- 不做分布式索引或云端索引。
- 不要求兼容旧 JSON 快照，允许全新索引格式。

---

## 2. 核心决策

| 决策点 | 结论 |
|---|---|
| 工程规模 | 按 5 万 Java 文件设计 |
| 功能范围 | 只做跳转，不做高级提示/补全 |
| 加密文件占比 | 90%，VS Code 内容桥为主路径 |
| 打开文件数量 | `< 100`，可在内存中常驻全文 |
| 变更判断 | 明文 `blake3` hash，不用密文 hash 判断语义变化 |
| 存储 | `redb` |
| 压缩 | `zstd`，按文件或小分片压缩 |
| 读模型 | `ArcSwap<IndexSnapshot>`，不可变快照 |
| 写模型 | 单写者 `IndexBuilder`，有界队列 |
| 文档同步 | `didChange` debounce 150~250ms |
| 首次建索引 | 两阶段：先符号/类型/方法签名，后 occurrences |
| 目录刷新 | 默认 `force = true`，人工兜底 |
| 解析器 | `tree-sitter` + `tree-sitter-java` |
| 位置转换 | `line-index` 处理 LSP UTF-16 行列 |
| 语义引擎 | 自研轻量索引，不引入 JDT/SCIP |

---

## 3. 总体架构

```text
VS Code Extension
├─ 用户命令 / QuickPick
├─ 配置同步
└─ VS Code Content Bridge
   │ 提供加密文件明文，只驻留内存
   ▼
LSP Server
├─ Protocol Adapter
├─ Document Store
│  ├─ 最新文本
│  ├─ document version
│  └─ debounce / coalescing
│
├─ Content Source
│  ├─ EditorTextSource
│  ├─ VSCodeBridgeSource
│  └─ DiskSource（仅 stat / 兜底）
│
├─ Indexing Actor
│  ├─ Event Queue（有界）
│  ├─ Single Writer / Index Builder
│  ├─ Bounded Parser Pool
│  ├─ FileContribution Builder
│  ├─ Dependency Resolver
│  ├─ MyBatis Namespace Linker
│  └─ Snapshot Publisher
│        │
│        ▼
│   ArcSwap<IndexSnapshot>
│
├─ Query Service
│  ├─ definition
│  ├─ references
│  ├─ implementations
│  ├─ method search
│  └─ MyBatis navigation
│
└─ Index Store
   ├─ redb
   ├─ per-file contributions
   ├─ symbol catalog
   ├─ type graph
   ├─ occurrence shards
   └─ metadata
```

---

## 4. 模块边界

### 4.1 `content`

职责：提供文件明文，隔离加密文件问题。

```rust
trait ContentSource {
    fn read(&self, file: FileId) -> Result<Arc<str>, ContentError>;
}
```

实现：

- `EditorTextSource`
  - 来自 `didOpen` / `didChange`
  - 适用于当前打开的 `< 100` 个文件
- `VSCodeBridgeSource`
  - 通过自定义 LSP 请求向 VS Code 请求明文
  - 90% 加密文件的主路径
- `DiskSource`
  - 只用于非加密文件或桥接失败时的兜底
  - 不用于判断明文语义变化

约束：

- 明文只在内存中出现；
- 不写入 redb；
- 不写入日志；
- 不出现在错误信息中。

---

### 4.2 `workspace-model`

职责：管理工作区和文件身份。

核心对象：

```text
Workspace
FileId(u32)
FilePath
FileMetadata
DocumentVersion
MavenModule
ClasspathEntry
```

要求：

- 从第一天就引入 `FileId`；
- 路径规范化；
- Maven 模块发现；
- 排除规则；
- 文件变化事件。

---

### 4.3 `java-analyzer`

职责：单文件纯解析。

```rust
trait FileAnalyzer {
    fn analyze(&self, file: FileId, source: &str) -> FileContribution;
}
```

输出：

```text
FileContribution
├─ symbols
├─ local occurrences
├─ unresolved occurrences
├─ type edges
└─ provider hints
```

要求：

- 无全局状态；
- 无磁盘访问；
- 可并行执行；
- 同一输入必须得到同一输出。

Java 分析器内部处理：

- 类型声明；
- 方法声明；
- 字段；
- 调用和引用候选；
- Lombok 合成符号；
- MyBatis namespace hint。

---

### 4.4 `index-builder`

职责：唯一写者，处理增量更新。

输入事件：

```text
FileChanged
FileDeleted
FileDiscovered
DocumentOpened
DocumentChanged
DocumentSaved
DocumentClosed
ConfigChanged
ShardLoaded
```

处理流程：

```text
事件去重 / 合并
→ 读取最新明文
→ 计算 plaintext hash
→ hash 未变：结束
→ 单文件解析
→ 计算受影响依赖
→ 生成 delta
→ 构建新 IndexSnapshot
→ ArcSwap 原子发布
→ 异步持久化
```

必须保证：

- 同一文件连续编辑只保留最后一次；
- 旧版本任务可取消或被新版本覆盖；
- 全局只有一个 builder 修改索引；
- 发布和持久化解耦；
- 持久化失败不影响当前查询。

---

### 4.5 `index-store`

职责：持久化与冷启动。

推荐使用 `redb`。

逻辑表结构：

```text
files
├─ file_id
├─ path
├─ size
├─ mtime_ns
├─ plaintext_hash
└─ state

contributions
├─ file_id
└─ zstd(postcard(FileContribution))

symbols
├─ symbol_id
├─ stable_key
├─ file_id
├─ kind
├─ name
├─ fqn
├─ owner
└─ descriptor

type_edges
├─ child_symbol
├─ parent_symbol
└─ edge_kind

occurrence_shards
├─ shard_id
├─ file range
└─ occurrence data

namespaces
├─ framework provider
├─ namespace
└─ affected files

metadata
├─ schema version
├─ workspace root
├─ index generation
└─ completeness
```

序列化建议：

```text
FileContribution / occurrence blob → postcard
外层压缩 → zstd
元数据 → redb 表
```

不要把全局符号目录存成一个大 blob；否则修改一个符号会导致大范围重写。

---

## 5. 数据模型

### 5.1 文件与符号身份

从零设计时直接使用：

```text
FileId(u32)
SymbolId(u64)
ShardId(u32)
```

`SymbolId` 建议由数据库或 interner 分配，而不是仅依赖字符串哈希。

稳定键：

```text
stable_key = fqn + descriptor + file_id
```

这样可以避免：

- 哈希冲突；
- 同名方法覆盖；
- 分片加载后 ID 不稳定。

---

### 5.2 Symbol

```text
Symbol
├─ id
├─ kind
├─ name: Arc<str>
├─ fqn: Arc<str>
├─ owner: Option<SymbolId>
├─ descriptor: Arc<str>
├─ declaration span
└─ flags
```

初期可以先用 `String`，后续再切换到 `Arc<str>`；但 `FileId` 和稳定 ID 应从第一天就存在。

---

### 5.3 Occurrence

```text
Occurrence
├─ symbol_id
├─ file_id
├─ span
├─ role
```

存储建议：

- 全局符号目录常驻；
- occurrence 按目录分片存储；
- 查询引用时只加载相关分片；
- 引用结果按 `(file_id, span)` 去重和排序。

---

### 5.4 TypeGraph

```text
TypeEdge
├─ child
├─ parent
├─ kind: extends / implements
```

同时维护两个方向的索引：

```text
children_by_parent
parents_by_child
```

从第一天就支持：

- 接口 → 实现；
- 实现 → 接口；
- 传递继承；
- 循环保护。

---

## 6. 变更识别与最小更新

### 6.1 文件元数据

每个文件维护：

```text
FileId
path
size
mtime_ns
plaintext_hash
parse_version
```

判断流程：

```text
stat / watcher / didChange
→ 拿到最新明文
→ 计算 plaintext hash
→ hash 没变：结束
→ hash 变了：解析这一个文件
→ FileContribution diff
→ 只更新受影响的索引项
→ 发布新快照
→ redb 只写这个文件的 blob
```

### 6.2 加密文件

90% 文件加密，因此：

- `VSCodeBridgeSource` 是主内容来源；
- 磁盘只做 `stat`；
- 不用密文 hash 判断语义变化；
- 明文 hash 才是权威判断。

### 6.3 打开文件

同时打开文件 `< 100`，因此：

```text
DocumentStore
├─ file_id
├─ latest text
├─ document version
└─ plaintext hash
```

编辑时：

```text
didChange
→ 更新内存文本
→ debounce 150~250ms
→ 只解析最后一次
```

### 6.4 目录刷新

提供两种模式：

#### 普通刷新

```text
refreshDirectory(path, force = false)
```

流程：

```text
遍历该目录
→ stat
→ stat 未变：跳过
→ stat 变化：读明文
→ hash 相同：跳过
→ hash 变化：重解析
```

#### 强制刷新

```text
refreshDirectory(path, force = true)
```

流程：

```text
遍历该目录
→ 忽略缓存 hash
→ 全部重新读取明文
→ 全部重解析
→ 重建该目录范围内的影响面
```

人工兜底命令默认使用 `force = true`。

---

## 7. 影响范围控制

需要维护反向依赖索引：

```text
unresolved_by_name
files_by_type
files_by_namespace
symbols_by_file
occurrences_by_file
```

不同变更的影响范围：

| 变更类型 | 影响范围 |
|---|---|
| 方法体修改 | 当前文件 occurrences |
| 方法改名 | old name + new name 的 unresolved refs |
| 类继承变化 | 对应类型子树 |
| MyBatis XML 变化 | 单个 namespace |
| 文件删除 | 该文件贡献 + 相关引用 |

目标是：

> 动一个文件，最多影响这个文件 + 少量依赖它的引用、类型边或 namespace。

---

## 8. 查询模型

所有查询入口：

```rust
struct QueryEngine {
    snapshots: Arc<ArcSwap<IndexSnapshot>>,
}

impl QueryEngine {
    fn definition(&self, file: FileId, offset: Offset) -> Vec<Location>;
    fn references(&self, symbol: SymbolId) -> Vec<Location>;
    fn implementations(&self, symbol: SymbolId) -> Vec<Symbol>;
    fn supertypes(&self, symbol: SymbolId) -> Vec<Symbol>;
    fn search_methods(&self, query: &str) -> Vec<MethodHit>;
}
```

查询必须：

- 只读；
- 不修改索引；
- 不触发分片加载；
- 不等待写锁；
- 使用同一版本快照。

---

## 9. MyBatis 与 Lombok

### 9.1 MyBatis

不要把 MyBatis 写死在索引核心里。

```rust
trait FrameworkProvider {
    fn id(&self) -> &'static str;

    fn analyze_file(
        &self,
        file: FileId,
        source: &str,
        symbols: &FileSymbols,
    ) -> FrameworkFileContribution;

    fn affected_keys(&self, contribution: &FrameworkFileContribution) -> Vec<FrameworkKey>;
}
```

增量规则：

```text
XML 变化
→ 只更新该 namespace

Java 方法改名
→ 处理旧 namespace + 新 namespace

Java 类型删除
→ 清理对应 namespace links
```

### 9.2 Lombok

Lombok 可以作为 Java analyzer 的内部 pass，但必须生成稳定 synthetic symbol。

---

## 10. 两阶段索引

首次建索引时：

### Phase 1

先建：

- 类型
- 方法签名
- 字段
- 继承边
- namespace 映射
- 方法搜索目录

目的：

- `definition`
- `implementation`
- `typeDefinition`
- 方法搜索
尽快可用。

### Phase 2

后台补全：

- 方法体 occurrences
- unresolved references
- highlight 数据

目的：

- `references`
- `documentHighlight`
逐步可用。

这样可以显著改善 5 万文件首次建索引时的首屏可用性。

---

## 11. 性能与可观测性

必须内置分段埋点：

```text
discover
stat
content_read
parse
resolve
links
snapshot_publish
persist
query_definition
query_references
query_implementations
```

`stats` 命令应输出：

- `files`
- `symbols`
- `occurrences`
- `loadedShards`
- `openDocuments`
- `indexGeneration`
- `lastBuildDuration`
- `rss`
- `pendingEvents`

目标是每一项优化都有可回归的量化基线。

---

## 12. 实施里程碑

### M0：架构契约

产出：

- `FileId`
- `SymbolId`
- `FileContribution`
- `IndexSnapshot`
- `ContentSource`
- 事件模型
- 最小测试 fixture

验收：

- 单文件解析纯函数测试通过。
- 查询接口可以返回假索引结果。

---

### M1：内容与文档模型

实现：

- `DocumentStore`
- `EditorTextSource`
- `VSCodeBridgeSource`
- `DiskSource`
- `plaintext_hash`

验收：

- 加密文件可通过 VS Code 桥获取明文。
- 打开文件编辑不读磁盘。
- 明文不落盘、不写日志。

---

### M2：不可变快照与查询

实现：

- `IndexSnapshot`
- `ArcSwap` 发布
- 只读查询接口
- definition / implementation / supertypes

验收：

- 编辑期间查询不失败。
- 查询不修改索引。
- 接口 ↔ 实现双向跳转可用。

---

### M3：redb 持久化

实现：

- `redb` 表结构
- `FileContribution` blob
- symbol catalog
- type graph
- metadata

验收：

- 重启后不全量扫描。
- 分片损坏可恢复。
- 索引版本升级自动重建。

---

### M4：增量编辑

实现：

- debounce
- per-file delta
- unresolved 局部重绑
- MyBatis namespace 增量

验收：

- 连续输入只解析最后一次。
- 方法改名后引用不残留旧结果。
- 编辑期间跳转不阻塞。

---

### M5：目录刷新与兜底

实现：

- `refreshDirectory(path, force)`
- 目录级删除/新增/变更
- watcher 兜底

验收：

- 指定目录强制刷新可用。
- 只影响该目录。
- 刷新结果可报告 scanned/changed/removed/failed。

---

### M6：两阶段索引

实现：

- Phase 1 符号目录
- Phase 2 occurrences 后台补全

验收：

- 首次建索引时 definition / implementation 先可用。
- references 逐步补全。
- 用户能感知索引阶段状态。

---

### M7：查询优化

实现：

- occurrence shard
- position 二分
- 继承图索引
- 方法搜索目录

验收：

- 5 万文件下查询延迟稳定。
- 引用查询不加载无关分片。
- 方法路径搜索响应可预测。

---

## 13. 用户测试检查点

每个阶段完成后，请用户实际测试：

### M1 后

- 打开加密 Java 文件，确认能读到明文。
- 编辑文件，确认不读磁盘。
- 关闭文件，确认明文被释放。

### M2 后

- 编辑期间执行 definition / implementation。
- 接口 ↔ 实现双向跳转。
- 查询不被写入阻塞。

### M3 后

- 重启 VS Code / LSP。
- 不做全量扫描。
- 冷启动时间可接受。

### M4 后

- 连续输入。
- 修改方法名。
- 删除文件。
- 确认引用更新正确。

### M5 后

- 执行指定目录强制刷新。
- 确认只影响该目录。
- watcher 失效时可用该命令恢复。

### M6 后

- 首次建索引期间先测试 definition / implementation。
- 等待后台补全 references。
- 观察阶段提示是否清晰。

### M7 后

- 在 5 万文件工程上测试：
  - definition
  - implementation
  - references
  - method search
  - MyBatis
- 记录 p50 / p95 延迟和 RSS。

---

## 14. 主要风险与对策

| 风险 | 对策 |
|---|---|
| 加密文件读取不稳定 | VS Code 桥主路径，批量 + 超时 + 失败重试 |
| 明文内容泄露 | 明文只驻留内存，不进 redb、不写日志 |
| 单写者成为瓶颈 | 有界队列 + 合并事件 + 后台持久化 |
| 快照构建成本过高 | per-file delta + 增量贡献 + 局部重绑 |
| 语义误绑定 | 符号稳定 ID + 明确依赖键 + 回归测试 |
| 首次建索引太慢 | 两阶段索引 + partial snapshot |
| 5 万文件内存过高 | occurrence 分片 + lazy 加载 + FileId |
| redb 写放大 | per-file blob + 小分片 + 后台写入 |

---

## 15. 当前推荐落地顺序

1. `FileId + DocumentStore + VSCodeBridge + plaintext hash`
2. `IndexSnapshot + ArcSwap`
3. `redb + FileContribution`
4. 单文件 delta
5. 指定目录强制刷新
6. 两阶段索引
7. 查询优化与性能埋点

---

## 16. 一句话总结

> **打开文件走编辑器文本，关闭文件走 VS Code 桥，用明文 hash 判断真实变化；索引用单写者 + 不可变快照，存储用 redb + zstd，查询只读。目标是 5 万文件下，跳转始终可用，编辑只影响局部。**
