# scala3-bsp-semantic-ls 项目 Rationale 与实现计划

> **SUPERSEDED (topology/toolchain)** — 2026-07: the v2 rewrite shipped. The
> single-process **Rust core + embedded JVM presentation-compiler island**
> topology, the storage idiom (immutable segments + `manifest.json` +
> generational workspace-state, **no SQLite**), and the toolchain contract are
> now normative in [plan-rust.md](plan-rust.md); feature semantics
> (consistency levels, groups, rename safety, §18.1) remain as specified here
> and in docs/architecture.md. Module/JVM/AOT/SQLite details below describe the
> deleted Scala implementation and are historical.

> 状态：设计基线草案  
> 日期：2026-07-02  
> 目标读者：项目 owner、核心实现者、后续 contributor  
> 核心定位：**Scala 3 + BSP + SemanticDB-first + Java 25 + SQLite/mmap 精确索引的高准确度 Language Server**

---

## 0. 一句话总结

`scala3-bsp-semantic-ls` 是一个只服务 **Scala 3 + BSP** 项目的专用 language server。它不追求 Metals 的通用性，而追求三个核心能力的高准确度和高性能：

1. `workspace/symbol`
2. 全仓库 `textDocument/references`
3. 跨文件 `textDocument/rename`

全局语义事实只来自 **scalac 生成的 SemanticDB**。SQLite 和 mmap postings 只是 SemanticDB 的物化索引。Scala 3 Presentation Compiler 只用于 completion、hover、signature help、definition、dirty buffer overlay 以及 PC-only 插件增强。

本项目工程管理必须使用：

```nix
mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher";
```

并且必须以 **Nix flake + Mill + mill-ivy-fetcher** 作为唯一正式开发、构建、依赖锁定和 CI 环境入口。

---

## 1. 硬性约束

### 1.1 语言与运行时约束

必须满足：

```text
Java 25 only
Scala 3 only
JVM only
BSP only
Mill project build
Nix flake controlled toolchain
No Scala Native
No Scala 2
No Java language server
```

Java 25 是项目唯一支持运行时。项目会直接使用 Java 25 的 FFM、MemorySegment mmap、AOT cache、Compact Object Headers、JFR profiling 等能力。

### 1.2 构建与依赖管理约束

项目必须使用：

```text
Nix flake
Mill
mill-ivy-fetcher
```

硬性规则：

```text
1. 项目必须提供 flake.nix 和 flake.lock。
2. flake inputs 必须包含：
   mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher";
3. 开发环境必须通过 nix develop 进入。
4. Mill 依赖必须通过 mill-ivy-fetcher 生成 Nix lock 表达式。
5. CI 必须通过 nix flake check / nix develop / mill 执行。
6. 不允许把本地 Coursier cache、系统 JDK、系统 SQLite 当作正式构建依赖。
7. 不允许用 sbt、Maven、Gradle 作为本项目自身构建工具。
```

`mill-ivy-fetcher` 的目标是把 Mill/Ivy 依赖转换成 Nix 表达式；它提供 `mif` 工具生成 lock 文件，并在 README 中推荐把 `mill-ivy-fetcher` 加入 flake inputs 和 overlays。它的 README 还说明当前要求 Nix >= 2.28，并支持 Mill 0.12.7+ 或 Mill 1.1.0+。见参考资料 [R7]。

### 1.3 语义事实约束

全局功能只信 SemanticDB：

```text
workspace symbol        -> SemanticDB index only
全仓库 references        -> SemanticDB index only, dirty-buffer PC overlay optional
跨文件 rename            -> fresh SemanticDB + mmap postings only
```

禁止作为全局事实源：

```text
Bloom filter
名字 grep
语法猜测
Metals V2 风格 source approximation
PC-generated persistent index
PC-only plugin synthetic symbol
```

### 1.4 插件边界

本项目不管理 SemanticDB compiler plugin。

```text
SemanticDB 插件 / scalac 插件：
  属于 build tool / BSP server / scalac 配置。
  用户必须在真实 build 中管理。
  本项目只消费 scalac-generated SemanticDB。

PC 插件：
  属于本项目管理范围。
  运行在 PC worker 中。
  只影响 PC 请求结果。
  不得写 SQLite。
  不得写 mmap postings。
  不得改变 workspace-wide semantic truth。
```

---

## 2. Rationale

### 2.1 为什么不是 Metals

Metals 是通用 Scala language server，支持多 Scala 版本、多 build tool、Java、scalafmt、scalafix、test/debug、worksheet、Bloop/sbt/Mill/Bazel 等复杂集成。

本项目目标相反：

```text
更窄：只支持 Scala 3 + BSP。
更硬：全局语义只来自 scalac SemanticDB。
更快：references / rename 使用 mmap exact postings。
更少 fallback：拒绝粗略结果，不用 source approximation。
```

这不是 Metals 替代品，而是一个为严格准确 references/rename 设计的专用 LS。

### 2.2 为什么 SemanticDB-first

SemanticDB 的 `TextDocument` 记录 `symbols`、`occurrences`、`diagnostics`、`synthetics`、`md5` 等信息；`SymbolOccurrence` 记录 source range、semantic symbol 和 role，role 区分 `REFERENCE` 与 `DEFINITION`。这些字段天然支撑 workspace symbol、references 和 rename。SemanticDB spec 还明确说明 Range 与 LSP Range 对应。见参考资料 [R1]。

因此，本项目的核心选择是：

```text
scalac-generated SemanticDB = truth
SQLite = truth 的 metadata/materialized cache
mmap postings = truth 的高速倒排索引
```

### 2.3 为什么不是 Metals V2 风格近似索引

Metals V2 的方向强调未编译也能尽快可用，因此会使用源码索引、候选筛选、Bloom/filter-like 思路以及 PC fallback。

本项目要求更严格：

```text
没有 fresh SemanticDB 就不做严格全局 rename。
没有 fresh SemanticDB 就不给出 pretend-accurate workspace-wide references。
用户要的是准确，而不是“可能对”。
```

因此性能只能来自 exact materialization、exact pruning、exact snapshot、exact mmap postings，而不是概率或语法近似。

### 2.4 为什么 SQLite + mmap postings hybrid

SQLite 擅长：

```text
metadata
manifest
transactions
FTS workspace symbol
small indexed lookups
schema evolution
debuggability
```

mmap postings 擅长：

```text
symbol/group -> occurrences
低延迟 references
低对象分配
顺序内存读取
rename edit set 生成
```

references 的热查询本质是：

```text
semantic group id -> occurrence list
```

这类访问不应通过 SQL VM、row materialization 和 B-tree 扫描来完成，而应通过只读 mmap postings 直接读取。

### 2.5 为什么 Java 25 only

Java 25 提供几个对本项目关键的能力：

```text
FFM API：调用 SQLite C API。
MemorySegment：结构化读取 mmap postings。
AOT cache：加速 LS cold start 和首个请求。
Compact Object Headers：降低对象密集 workload 的堆压力。
JFR profiling：内建性能诊断。
```

Oracle JDK 25 文档列出 Compact Object Headers、AOT cache ergonomics、AOT method profiling、JFR CPU-time profiling 等运行时和性能改进。见参考资料 [R5]。

---

## 3. 目标与非目标

### 3.1 必须实现的 LSP 能力

```text
initialize / shutdown
textDocument/didOpen
textDocument/didChange
textDocument/didSave
textDocument/completion
completionItem/resolve
textDocument/hover
textDocument/signatureHelp
textDocument/definition
textDocument/typeDefinition
workspace/symbol
textDocument/references
textDocument/rename
textDocument/documentHighlight
textDocument/semanticTokens, later
textDocument/inlayHint, later
workspace/executeCommand for doctor / reindex / compile / plugin status
```

### 3.2 必须实现的全局能力

```text
workspace symbol:
  SQLite FTS + SemanticDB SymbolInformation only.

references:
  doc-postings symbol-at-cursor + ref_group mmap postings.

rename:
  fresh compile + md5 validation + editable rename postings + safety profile.
```

### 3.3 非目标

```text
Scala 2
Java LS
Scala Native
Scala.js / Native 初版支持
non-BSP build import
scalafmt / scalafix
test explorer / debug adapter 初版支持
worksheet / ammonite
Metals Doctor compatibility
PC-generated persistent semantic index
SemanticDB compiler plugin management
```

---

## 4. 外部协议与契约

### 4.1 BSP 契约

本项目是 LSP server，同时作为 BSP client。

最低 BSP endpoints：

```text
build/initialize
workspace/buildTargets
buildTarget/sources
buildTarget/scalacOptions
buildTarget/compile
build/publishDiagnostics
```

强烈推荐：

```text
buildTarget/inverseSources
buildTarget/dependencySources
buildTarget/outputPaths
buildTarget/didChange
```

BSP 的目标是让工具开发者不必为每个 build tool 单独实现 source layout、compiler options 等集成；BSP spec 也说明 LSP server 可以作为 BSP client。见参考资料 [R2]。

### 4.2 Scala 3 SemanticDB 契约

每个可索引 target 必须能定位：

```text
semanticdb output directory
sourceroot
source files
classpath
scalac options
```

本项目不会替 build server 注入 SemanticDB scalac plugin，也不会管理 SemanticDB-side compiler plugin。缺少 SemanticDB 的 target 标记为：

```text
IndexUnavailable
```

该 target 上禁用：

```text
workspace symbol
workspace references
cross-file rename
```

PC 仍可提供 completion/hover 等编辑期功能。

### 4.3 PC 契约

PC 用于：

```text
completion
hover
signature help
definition / typeDefinition
dirty buffer symbol-at-cursor overlay
prepareRename
PC-only plugin effects
```

PC 不用于：

```text
persistent indexing
global references truth
cross-file rename truth
SQLite writes
mmap postings writes
```

---

## 5. 总体架构

```text
Editor / IDE
  ⇅ LSP
scala3-bsp-semantic-ls, Java 25
  ├─ LSP Layer
  ├─ BSP Client
  ├─ Project Model
  ├─ SemanticDB Locator
  ├─ SemanticDB Ingestor
  ├─ SQLite FFM Metadata Store
  ├─ Mmap Postings Store
  ├─ Snapshot Manager
  ├─ Query Orchestrator
  ├─ PC Worker Manager
  ├─ PC Plugin Manager
  ├─ Rename Safety Engine
  ├─ Doctor
  └─ Bench / JFR Harness
```

### 5.1 Main LS JVM

职责：

```text
LSP protocol
BSP protocol
SemanticDB scanning / ingest
SQLite metadata
mmap postings snapshot
workspace symbol
references
rename
diagnostics forwarding
project state
index state
doctor
```

### 5.2 PC Worker JVM

职责：

```text
Scala 3 Presentation Compiler
PC compiler plugin loading
PC service plugin loading
synthetic source provider
dirty buffer overlay
completion / hover / signature / definition
prepareRename
```

PC worker 与主进程隔离。用户插件崩溃不能破坏主索引。

### 5.3 Storage

```text
.scala3-bsp-semantic-ls/
  meta.sqlite
  meta.sqlite-wal
  meta.sqlite-shm

  postings/
    segment-000001/
      header.bin
      ref-group-index.bin
      rename-group-index.bin
      doc-index.bin
      ref-postings.bin
      definition-postings.bin
      rename-postings.bin
      doc-postings.bin
      block-index.bin

  snapshots/
    current.json

  pc/
    plugins/
    generated-sources/
```

---

## 6. 数据模型

### 6.1 ID 分层

```text
stable_symbol_id:
  SQLite 中持久存在。

snapshot_symbol_ord:
  当前 snapshot 内 dense id，用于 O(1) array lookup。

doc_id:
  SQLite 中持久 document id。

doc_ord:
  当前 snapshot 内 dense document id。

target_id:
  SQLite 中持久 target id。

target_ord:
  当前 snapshot 内 dense target id。

ref_group_id / ref_group_ord:
  references 使用的 exact alias group。

rename_group_id / rename_group_ord:
  rename 使用的 exact editable group。
```

SemanticDB global symbol 在一个 universe 中才应唯一；local symbol 必须结合 document 使用。SemanticDB spec 明确建议在需要唯一性时为 global/local symbols 搭配额外 metadata。见参考资料 [R1]。

### 6.2 Alias group

必须分开：

```text
reference group
rename group
```

原因：references 可以合并的 symbol，rename 未必可以安全合并。

需要处理：

```text
class vs constructor
class vs companion object
val getter
var getter / setter
apply / unapply
method overload
extension method
opaque type
exported symbol
override family
```

初版策略：

```text
references:
  尽量完整 exact alias group。

rename:
  保守 group。
  unsafe family 直接拒绝。
```

---

## 7. SQLite schema

SQLite 只负责 metadata、manifest、FTS、dictionary 和小查询。不要把 references 热路径放在 SQLite occurrence 表中。

### 7.1 targets

```sql
CREATE TABLE targets (
  target_id        INTEGER PRIMARY KEY,
  bsp_id           TEXT NOT NULL UNIQUE,
  scala_version    TEXT NOT NULL,
  classpath_hash   TEXT NOT NULL,
  options_hash     TEXT NOT NULL,
  semanticdb_root  TEXT NOT NULL,
  sourceroot       TEXT NOT NULL,
  active           INTEGER NOT NULL
);
```

### 7.2 documents

```sql
CREATE TABLE documents (
  doc_id               INTEGER PRIMARY KEY,
  target_id            INTEGER NOT NULL,
  uri                  TEXT NOT NULL,
  semanticdb_path      TEXT NOT NULL,
  semanticdb_mtime_ms  INTEGER NOT NULL,
  md5                  TEXT NOT NULL,
  epoch                INTEGER NOT NULL,
  active               INTEGER NOT NULL,
  generated            INTEGER NOT NULL DEFAULT 0,
  readonly             INTEGER NOT NULL DEFAULT 0,
  UNIQUE(target_id, uri)
);
```

### 7.3 symbol_intern

```sql
CREATE TABLE symbol_intern (
  symbol_id         INTEGER PRIMARY KEY,
  universe_id       INTEGER NOT NULL,
  semantic_symbol   TEXT NOT NULL,
  local_doc_id      INTEGER,
  stable_hash       INTEGER NOT NULL,
  UNIQUE(universe_id, semantic_symbol, local_doc_id)
);
```

### 7.4 symbol_metadata

```sql
CREATE TABLE symbol_metadata (
  symbol_id       INTEGER NOT NULL,
  target_id       INTEGER NOT NULL,
  doc_id          INTEGER NOT NULL,
  display_name    TEXT NOT NULL,
  owner_name      TEXT,
  package_name    TEXT,
  kind            INTEGER NOT NULL,
  properties      INTEGER NOT NULL,
  signature_hash  INTEGER,
  start_line      INTEGER,
  start_char      INTEGER,
  end_line        INTEGER,
  end_char        INTEGER,
  PRIMARY KEY(symbol_id, target_id, doc_id)
);
```

### 7.5 reference / rename groups

```sql
CREATE TABLE ref_groups (
  ref_group_id INTEGER PRIMARY KEY
);

CREATE TABLE rename_groups (
  rename_group_id INTEGER PRIMARY KEY,
  unsafe_reason_mask INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE symbol_to_ref_group (
  symbol_id INTEGER PRIMARY KEY,
  ref_group_id INTEGER NOT NULL
);

CREATE TABLE symbol_to_rename_group (
  symbol_id INTEGER PRIMARY KEY,
  rename_group_id INTEGER NOT NULL
);
```

### 7.6 workspace symbol FTS

```sql
CREATE VIRTUAL TABLE workspace_symbols_fts
USING fts5(
  display_name,
  owner_name,
  package_name,
  content=''
);

CREATE TABLE workspace_symbol_rows (
  rowid      INTEGER PRIMARY KEY,
  symbol_id  INTEGER NOT NULL,
  target_id  INTEGER NOT NULL,
  doc_id     INTEGER NOT NULL,
  kind       INTEGER NOT NULL
);
```

SQLite FTS5 是 full-text search virtual table module，适合 workspace symbol 搜索。见参考资料 [R8]。

### 7.7 segment manifest

```sql
CREATE TABLE segment_manifest (
  segment_id      INTEGER PRIMARY KEY,
  path            TEXT NOT NULL,
  created_at_ms   INTEGER NOT NULL,
  min_epoch       INTEGER NOT NULL,
  max_epoch       INTEGER NOT NULL,
  active          INTEGER NOT NULL,
  checksum        INTEGER NOT NULL
);
```

---

## 8. mmap postings 设计

### 8.1 设计原则

```text
Immutable segments
Snapshot swap
No in-place update
No lock on read path
Exact postings only
No Bloom
No grep
No syntax approximation
```

### 8.2 文件布局

```text
segment-N/
  header.bin
  ref-group-index.bin
  definition-group-index.bin
  rename-group-index.bin
  doc-index.bin
  ref-postings.bin
  definition-postings.bin
  rename-postings.bin
  doc-postings.bin
  block-index.bin
  checksums.bin
```

### 8.3 Header

```c
struct Header {
  uint32 magic;
  uint16 version;
  uint16 flags;
  uint64 segment_id;
  uint64 created_at_ms;
  uint64 ref_group_count;
  uint64 rename_group_count;
  uint64 doc_count;
  uint64 occurrence_count;
  uint64 checksum;
}
```

### 8.4 Group index

Dense ordinal direct lookup：

```c
struct GroupIndexEntry {
  int64 offset;
  int32 count;
  int32 block_index_offset;
}
```

查询：

```text
entry = refGroupIndex[ref_group_ord]
```

无需 binary search。

### 8.5 Columnar postings

首版即采用 columnar layout：

```text
doc_ord[]
doc_epoch[]
target_ord[]
packed_start[]
packed_end[]
flags[]
```

好处：

```text
批量过滤时只读取必要列。
减少 cache miss。
减少 JVM object allocation。
便于后续 Vector API 优化。
```

### 8.6 分离 postings

物化：

```text
ref_group_ord -> reference occurrences
ref_group_ord -> definition occurrences
rename_group_ord -> editable occurrences
doc_ord -> all occurrences
doc_ord -> editable occurrences
```

这样：

```text
references(includeDeclaration=false):
  只读 reference postings。

references(includeDeclaration=true):
  reference postings + definition postings。

rename:
  只读 editable postings。
```

### 8.7 doc-postings interval block index

用于：

```text
uri + position -> symbol occurrence
```

结构：

```text
doc_ord -> block index
block: firstLine, lastLine, offset, count
```

查询：

```text
position line
  -> find block
  -> scan small block
  -> occurrence covering position
```

### 8.8 block-level exact skip metadata

每个 postings block 存：

```text
target exact bitset
role counts
editable counts
doc min/max
generation/epoch range
```

用于 exact skip：

```text
if block.targetBitset ∩ allowedTargets == empty:
  skip

if rename and block.editable_count == 0:
  skip
```

这是精确集合，不是概率结构。

---

## 9. Snapshot 与一致性

### 9.1 Snapshot model

```text
AtomicReference[IndexSnapshot] currentSnapshot
```

请求开始：

```scala
val snapshot = currentSnapshot.retain()
try query(snapshot)
finally snapshot.release()
```

### 9.2 写入流程

```text
1. BSP compile succeeds.
2. SemanticDB watcher finds changed files.
3. RawSemanticDBPath parses TextDocuments.
4. Validate md5.
5. SQLite transaction interns symbols and updates metadata.
6. Build new postings segment in temp directory.
7. fsync segment.
8. SQLite manifest transaction marks segment active.
9. mmap new segment.
10. publish new snapshot.
11. old snapshot released after readers finish.
```

### 9.3 epoch filtering

每条 occurrence record 包含：

```text
doc_epoch
```

查询时必须检查：

```text
occ.doc_epoch == documents[doc_ord].epoch
```

旧 segment 中残留 occurrence 会被忽略。compactor 后台清理。

---

## 10. Query Orchestrator

三条 path：

```text
IndexPath:
  SQLite + mmap postings。

RawSemanticDBPath:
  stale/missing 时读 .semanticdb，验证后 write-through。

PCPath:
  dirty buffer 和编辑期功能。
```

一致性级别：

```text
BestEffort:
  workspace symbol。

FreshPreferred:
  references。

FreshRequired:
  rename。
```

---

## 11. workspace symbol

主路径：

```text
workspace/symbol(query)
  -> SQLite FTS5
  -> join symbol metadata / documents
  -> return WorkspaceSymbol[]
```

dirty buffer overlay：

```text
打开但未保存文件中的 PC-only symbols 可以作为临时 overlay。
必须标记为 PC-only。
不写入 SQLite/postings。
```

PC-only workspace symbol 不支持全局 references/rename。

---

## 12. 全仓库 references

流程：

```text
1. symbol-at-cursor
2. symbol -> ref_group
3. allowed target pruning
4. mmap ref postings 查询
5. optional dirty buffer PC overlay
6. dedupe
7. return LSP Location[]
```

### 12.1 symbol-at-cursor

```text
dirty file:
  PCPath wins。

clean fresh file:
  doc-postings mmap wins。

index stale/missing:
  RawSemanticDBPath reads .semanticdb, validates md5, writes through。
```

### 12.2 target graph exact pruning

BSP build target graph 给出依赖关系。

```text
definition target T
  -> allowed reference targets = T + reverseDependencyClosure(T)
```

这是由 build graph 得到的 exact upper bound，不是近似。

### 12.3 references pseudocode

```scala
def references(uri: URI, pos: Position, includeDeclaration: Boolean): List[Location] =
  val snap = currentSnapshot.retain()
  try
    val occ = symbolAtCursor(uri, pos, snap)
    val refGroup = snap.refGroupOf(occ.symbolOrd)
    val allowedTargets = snap.reverseDepsOf(occ.definitionTarget)

    val refs = snap.refPostings(refGroup)
      .filterTarget(allowedTargets)
      .filterEpoch()
      .toLocations()

    val defs =
      if includeDeclaration then snap.definitionPostings(refGroup).toLocations()
      else Nil

    val overlay = pcDirtyBufferOverlay(refGroup)
    dedupe(refs ++ defs ++ overlay)
  finally
    snap.release()
```

---

## 13. 跨文件 rename

rename 使用 `FreshRequired`。

流程：

```text
1. PC prepareRename
2. dirty buffer check
3. BSP buildTarget/compile affected target domain
4. ingest fresh SemanticDB
5. publish fresh snapshot
6. symbol-at-cursor
7. symbol -> rename_group
8. read editable rename postings
9. safety validation
10. produce WorkspaceEdit
```

BSP spec 明确说明 `buildTarget/compile` 可以在 `textDocument/rename` 前使用，以确保 workspace sources typecheck 且 up-to-date。见参考资料 [R2]。

### 13.1 rename safety rules

必须满足：

```text
fresh compile succeeded
fresh SemanticDB available
source md5 matches SemanticDB md5
all edits are workspace sources
no readonly sources
no dependency sources
no generated sources by default
no PC-only symbol
no synthetic-only occurrence
no unsafe override family by default
shared-source targets agree on same rename group
```

### 13.2 RenameProfile

ingest 阶段预计算：

```scala
case class RenameProfile(
  isLocal: Boolean,
  isExternal: Boolean,
  hasGeneratedOccurrences: Boolean,
  hasReadonlyOccurrences: Boolean,
  hasOverrideFamily: Boolean,
  hasCompanion: Boolean,
  editableOccurrenceCount: Int,
  unsafeReasonMask: Long
)
```

请求时：

```text
unsafeReasonMask != 0 -> reject with concrete reason
```

---

## 14. PC 与 PC 插件系统

### 14.1 PC core responsibilities

```text
completion
completionItem/resolve
hover
signature help
definition
typeDefinition
prepareRename
dirty buffer semantic overlay
PC diagnostics, optional and secondary
```

### 14.2 PC compiler plugin

用户可以配置 PC-only Scala 3 compiler plugin。

```text
-Xplugin:/path/to/plugin.jar
-P:plugin:key:value
```

它只进入 PC worker，不进入 build server，不进入 SemanticDB generation。

目的：

```text
模拟 macro/framework/compiler-plugin 在编辑期对 PC 的影响。
插入 PC 里的 compiler pass。
修复 PC typecheck/completion/hover 体验。
```

### 14.3 PC service plugin

项目定义稳定 SPI：

```scala
trait PcServicePlugin:
  def id: String
  def initialize(ctx: PcPluginInitContext): Unit = ()
  def patchOptions(ctx: PcTargetContext, options: Vector[String]): Vector[String] = options
  def patchSourcePath(ctx: PcTargetContext, sourcePath: Vector[Path]): Vector[Path] = sourcePath
  def syntheticSources(ctx: PcTargetContext): Vector[VirtualSource] = Vector.empty
  def beforeRequest(req: PcRequest): PcRequest = req
  def afterCompletion(req: PcRequest, result: CompletionList): CompletionList = result
  def afterHover(req: PcRequest, result: Option[Hover]): Option[Hover] = result
  def afterDefinition(req: PcRequest, result: DefinitionResult): DefinitionResult = result
  def filterPcDiagnostics(req: PcRequest, diagnostics: Vector[Diagnostic]): Vector[Diagnostic] = diagnostics
```

### 14.4 插件权限矩阵

| 功能 | PC 插件是否可影响 | 说明 |
|---|---:|---|
| completion | 是 | 编辑期功能 |
| hover | 是 | 可增强解释 |
| signature help | 是 | 可增强 DSL/macro 体验 |
| definition | 是，但标记来源 | 可跳 synthetic source |
| PC diagnostics | 是 | build diagnostics 仍是主诊断 |
| dirty buffer references overlay | 有限 | 不写持久索引 |
| workspace symbol | 否 | 只来自 SemanticDB index |
| 全仓库 references | 否 | 只来自 SemanticDB/postings |
| 跨文件 rename | 否 | PC 只做 prepareRename |
| SQLite/postings 写入 | 禁止 | 只能来自 scalac SemanticDB |

### 14.5 PC-only symbol 规则

如果 symbol 只存在于 PC 插件提供的 synthetic source 或 overlay：

```text
completion/hover/definition 可以工作。
workspace references 不承诺。
cross-file rename 拒绝。
```

错误信息：

```text
This symbol is provided by a PC-only plugin and is not present in fresh SemanticDB.
Workspace-wide references and cross-file rename are unavailable for this symbol.
```

---

## 15. Nix flake + Mill + mill-ivy-fetcher 工程设计

本节是项目硬性工程约束，不是可选建议。

### 15.1 目录结构

```text
.
├── flake.nix
├── flake.lock
├── build.mill
├── mill
├── nix/
│   ├── ivy-lock.nix
│   ├── package.nix
│   ├── dev-shell.nix
│   └── checks.nix
├── modules/
│   ├── ls-core/
│   ├── ls-bsp/
│   ├── ls-semanticdb/
│   ├── ls-index-model/
│   ├── ls-sqlite-ffm/
│   ├── ls-postings/
│   ├── ls-pc/
│   ├── ls-rename/
│   ├── ls-doctor/
│   └── ls-bench/
└── docs/
    ├── architecture.md
    ├── index-format.md
    ├── plugin-spi.md
    └── nix-build.md
```

### 15.2 flake.nix skeleton

```nix
{
  description = "scala3-bsp-semantic-ls";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    # Hard requirement.
    mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher";
  };

  outputs = { self, nixpkgs, flake-utils, mill-ivy-fetcher }@inputs:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            mill-ivy-fetcher.overlays.default
            mill-ivy-fetcher.overlays.mill-overlay
          ];
        };

        jdk = pkgs.jdk25;
        mill = pkgs.millVersions.mill_1_1_2 or pkgs.mill;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            jdk
            mill
            mill-ivy-fetcher
            sqlite
            pkg-config
            git
            jq
          ];

          JAVA_HOME = "${jdk}";
          LS_JAVA_VERSION = "25";
        };

        formatter = pkgs.nixpkgs-fmt;

        checks = {
          # Implement in nix/checks.nix once Mill targets are stable.
        };

        packages.default = pkgs.callPackage ./nix/package.nix {
          inherit mill jdk;
          inherit (pkgs) sqlite;
        };
      }) // { inherit inputs; };
}
```

说明：

```text
1. mill-ivy-fetcher input 必须存在。
2. dev shell 必须使用 Java 25。
3. Mill 版本必须来自 flake environment。
4. SQLite 必须来自 Nix package set。
5. CI 与本地开发必须同一路径。
```

### 15.3 依赖锁定工作流

推荐命令：

```bash
nix develop
mif run -p . -o nix/ivy-lock.nix
```

或者按 `mill-ivy-fetcher` README 推荐方式：

```bash
nix shell '.#mill' '.#mill-ivy-fetcher' -c mif run -p . -o nix/ivy-lock.nix
```

规则：

```text
1. 修改 build.mill 依赖后必须重新生成 nix/ivy-lock.nix。
2. PR 必须同时提交 build.mill 和 nix/ivy-lock.nix。
3. CI 必须验证 ivy-lock.nix 与 build.mill 一致。
4. 不允许 CI 在线临时解析未锁定依赖。
```

### 15.4 Mill module layout

`build.mill` 应按模块拆分：

```scala
object core extends ScalaModule
object bsp extends ScalaModule
object semanticdb extends ScalaModule
object indexModel extends ScalaModule
object sqliteFfm extends ScalaModule
object postings extends ScalaModule
object pc extends ScalaModule
object rename extends ScalaModule
object doctor extends ScalaModule
object bench extends ScalaModule
```

公共规则：

```text
scalaVersion: Scala 3.x, exactly pinned
javacOptions: --release 25 where applicable
semanticdb generation: only for project self-analysis/testing, not runtime truth
native access tests: run only under Java 25
```

### 15.5 CI commands

CI 最小集合：

```bash
nix flake check
nix develop -c mill __.compile
nix develop -c mill __.test
nix develop -c mill bench.smoke
nix develop -c ./scripts/check-ivy-lock.sh
```

### 15.6 Nix package output

package 输出应包含：

```text
bin/scala3-bsp-semantic-ls
lib/scala3-bsp-semantic-ls/*
share/scala3-bsp-semantic-ls/default-plugin-schema.json
```

启动 wrapper 必须设置：

```text
JAVA_HOME pointing to JDK 25
--enable-native-access=ls.sqlite.ffm
-XX:+UseCompactObjectHeaders
optional -XX:AOTCache=...
```

---

## 16. Java 25 runtime design

### 16.1 SQLite FFM

模块：

```text
ls-sqlite-ffm
```

调用：

```text
sqlite3_open_v2
sqlite3_prepare_v3
sqlite3_bind_*
sqlite3_step
sqlite3_column_*
sqlite3_reset
sqlite3_clear_bindings
sqlite3_finalize
sqlite3_close_v2
```

启动：

```bash
--enable-native-access=ls.sqlite.ffm
```

Java 25 FFM API 允许 Java 调用 JVM 外部 native code 和访问外部 memory。见参考资料 [R4]。

### 16.2 mmap postings

使用：

```text
FileChannel.map -> MemorySegment
read-only MemorySegment
snapshot-owned Arena
```

Java 25 文档说明可以把文件区域映射为 `MemorySegment`。见参考资料 [R6]。

### 16.3 AOT cache

训练运行覆盖：

```text
LSP initialize
BSP initialize
SQLite FFM open
prepare hot statements
mmap snapshot load
SemanticDB parse batch
workspace/symbol
references
PC initialize
completion
```

生产 wrapper 支持：

```bash
-XX:AOTCache=.scala3-bsp-semantic-ls/aot-cache.bin
```

---

## 17. 性能设计清单

必须从第一版实现：

```text
1. dense snapshot ordinals
2. exact ref_group / rename_group
3. role-separated postings
4. editable rename postings
5. doc-postings interval index
6. target graph exact pruning
7. block-level exact skip metadata
8. immutable segments
9. snapshot retain/release
10. batch SemanticDB ingest
11. batch symbol interning
12. SQLite FTS5 workspace symbol
13. Java 25 FFM SQLite binding
14. Java 25 MemorySegment mmap
15. JFR benchmark harness
16. compactor
```

禁止性能捷径：

```text
Bloom filter for correctness
source token grep
syntax-only references
PC-generated persistent global index
```

---

## 18. Testing plan

### 18.1 Correctness tests

必须覆盖：

```text
class references
object references
trait references
enum references
constructor references
companion class/object
method overload
val getter
var getter/setter
local val / local def
private member
top-level definitions
extension methods
given / using
export
inline
macro-generated API when present in build SemanticDB
shared sources across targets
generated sources
readonly source rejection
dependency source rejection
stale md5 rejection
compile failure rename rejection
PC-only symbol rename rejection
```

### 18.2 Index invariants

```text
segment manifest points to existing files
checksum validates
every active occurrence has valid doc_ord
stale doc_epoch is ignored
snapshot swap never exposes partial segment
compaction preserves occurrence set
SQLite metadata agrees with postings manifest
rename editable postings exclude readonly/generated/dependency sources
```

### 18.3 Performance tests

```text
cold start
warm start
BSP import
SemanticDB ingest 1k / 10k / 100k docs
workspace symbol prefix / fuzzy
references rare / medium / hot symbols
rename small / large
PC completion P50/P95/P99
PC plugin overhead
SQLite FFM overhead
mmap scan records/sec
```

### 18.4 Nix/Mill tests

```text
nix flake check
nix develop launches Java 25
mill compile under nix develop
mif lock refresh check
no unresolved Ivy dependency outside nix lock
package wrapper uses Java 25
```

---

## 19. Doctor

`workspace/executeCommand: scala3SemanticLs.doctor` 输出：

```text
Runtime:
  Java: 25.x
  Native access: enabled for ls.sqlite.ffm
  Compact Object Headers: enabled/disabled
  AOT cache: loaded/missing

Nix:
  flake detected: yes
  mill-ivy-fetcher input: yes
  ivy lock: nix/ivy-lock.nix
  lock status: fresh/stale

BSP:
  server: ...
  targets: ...
  Scala 3 targets: ...
  IndexUnavailable targets: ...

SemanticDB:
  semanticdb roots
  stale docs
  md5 mismatches
  generated source status

SQLite:
  WAL: enabled
  FTS: enabled
  manifest generation: ...

Postings:
  active segments: ...
  snapshot id: ...
  compaction pending: ...

PC:
  worker status
  active targets
  plugin status

PC Plugins:
  compiler plugins loaded
  service plugins loaded
  self-test results
  disabled plugins
```

---

## 20. Implementation plan

### Phase 0 — Spec freeze

产物：

```text
LSP capability spec
BSP requirement spec
SemanticDB freshness contract
SQLite schema v1
mmap postings binary format v1
PC plugin SPI v1
Nix flake contract
Mill module layout
```

验收：

```text
docs/architecture.md
docs/index-format.md
docs/plugin-spi.md
docs/nix-build.md
```

### Phase 1 — Nix + Mill foundation

实现：

```text
flake.nix
flake.lock
build.mill
nix/ivy-lock.nix
nix/package.nix
nix/checks.nix
scripts/check-ivy-lock.sh
```

验收：

```bash
nix develop
nix flake check
nix develop -c mill __.compile
nix develop -c mif run -p . -o nix/ivy-lock.nix
```

### Phase 2 — BSP project model

实现：

```text
.bsp discovery
build/initialize
workspace/buildTargets
buildTarget/sources
buildTarget/scalacOptions
buildTarget/inverseSources fallback
target graph
uri -> target mapping
SemanticDB output discovery
```

验收：

```text
真实 BSP project 能输出 target/source/classpath/scalacOptions/semanticdb report。
```

### Phase 3 — SemanticDB ingest model

实现：

```text
SemanticDB locator
protobuf parser
md5 validation
TextDocument normalization
symbol key model
local/global handling
SymbolInformation extraction
SymbolOccurrence extraction
alias group builder
rename profile builder
```

验收：

```text
给定 SemanticDB corpus，输出 deterministic normalized semantic model。
```

### Phase 4 — SQLite FFM metadata store

实现：

```text
FFM sqlite binding
connection pool
prepared statements
WAL setup
schema migration
symbol interning
documents/targets/symbol metadata
workspace_symbols_fts
segment manifest
```

验收：

```text
workspace/symbol 可从 SQLite FTS 返回。
SQLite writes only from SemanticDB ingest。
```

### Phase 5 — mmap postings store

实现：

```text
segment writer
segment reader
MemorySegment mmap
dense ordinals
ref postings
definition postings
rename postings
doc postings
block index
checksums
snapshot manager
snapshot retain/release
```

验收：

```text
references 查询不走 SQLite occurrence table。
symbol-at-cursor 能走 doc-postings。
```

### Phase 6 — references engine

实现：

```text
symbol-at-cursor
doc-postings interval lookup
ref_group lookup
target graph pruning
mmap postings scan
dirty buffer overlay hook
dedupe
LSP Location conversion
```

验收：

```text
全仓库 references correctness suite 通过。
```

### Phase 7 — rename engine

实现：

```text
prepareRename integration
compile-before-rename
fresh SemanticDB ingest
rename_group lookup
editable postings scan
safety profile check
md5 verification
WorkspaceEdit generation
shared source consistency check
```

验收：

```text
rename correctness suite 通过。
unsafe cases 明确拒绝。
```

### Phase 8 — PC worker

实现：

```text
PC worker process protocol
target -> PC instance
didOpen/didChange/didClose
completion
completion resolve
hover
signature help
definition/typeDefinition
prepareRename
dirty overlay
```

验收：

```text
基本编辑体验可用。
PC 结果不会写 SQLite/postings。
```

### Phase 9 — PC plugin system

实现：

```text
PC compiler plugin config
PC options patching
compiler plugin self-test
PC service plugin SPI
synthetic sources
completion/hover/definition hooks
diagnostics filtering
plugin fail policy
plugin doctor
```

验收：

```text
插件崩溃不影响主 LS。
PC-only symbol 不能做全局 references/rename。
```

### Phase 10 — compaction + performance

实现：

```text
segment compactor
hot group compaction
stale segment cleanup
SQLite checkpoint scheduling
JFR presets
AOT training mode
benchmark suite
```

验收：

```text
性能 benchmark 有稳定 P50/P95/P99 报告。
compaction 后 occurrence set 不变。
```

---

## 21. Risks and mitigations

### 风险：BSP server 不产出 SemanticDB

处理：

```text
IndexUnavailable
Doctor 报告缺失项
不 fallback 到粗略索引
```

### 风险：SemanticDB stale

处理：

```text
md5 check
doc epoch check
compile-before-rename
stale target status
```

### 风险：PC 插件与真实 build 不一致

处理：

```text
SemanticDB 仍是 truth
PC 插件只影响编辑期
PC-only symbol 禁止全局 rename
```

### 风险：mmap segment 与 SQLite manifest 不一致

处理：

```text
atomic rename
fsync
checksum
manifest transaction
startup recovery
```

### 风险：rename 误改

处理：

```text
FreshRequired
editable postings only
rename profile
shared-source consistency check
unsafe family rejection
```

### 风险：Nix/Mill lock 漂移

处理：

```text
CI check ivy-lock.nix
PR 必须提交 lock 更新
nix flake check 作为 gate
```

---

## 22. 参考资料

- [R1] SemanticDB Specification, Scalameta.  
  https://scalameta.org/docs/semanticdb/specification.html

- [R2] Build Server Protocol Specification.  
  https://build-server-protocol.github.io/docs/specification.html

- [R3] BSP Scala extension: scalacOptions and Scala target metadata.  
  https://build-server-protocol.github.io/docs/extensions/scala

- [R4] Java 25 Foreign Function & Memory API documentation.  
  https://docs.oracle.com/en/java/javase/25/core/foreign-function-and-memory-api.html

- [R5] Oracle JDK 25 significant changes, including Compact Object Headers, AOT cache ergonomics, AOT profiling, and JFR improvements.  
  https://docs.oracle.com/en/java/javase/25/migrate/significant-changes-jdk-25.html

- [R6] Java 25 file-backed MemorySegment documentation.  
  https://docs.oracle.com/en/java/javase/25/core/backing-memory-segment-memory-region-inside-file.html

- [R7] Avimitin/mill-ivy-fetcher README.  
  https://github.com/Avimitin/mill-ivy-fetcher

- [R8] SQLite FTS5 documentation.  
  https://www.sqlite.org/fts5.html

- [R9] SQLite WAL documentation.  
  https://sqlite.org/wal.html

- [R10] SQLite threading modes documentation.  
  https://sqlite.org/threadsafe.html

---

## 23. 最终设计原则

本项目最终必须保持以下形态：

```text
BSP provides project facts.
scalac SemanticDB provides semantic facts.
SQLite provides metadata, FTS, manifest, and dictionaries.
mmap postings provide exact high-speed reference/rename lookup.
Scala 3 PC provides interactive editing.
PC plugins improve PC only.
Nix flake + Mill + mill-ivy-fetcher provide reproducible build and dependency management.
```

最关键的边界：

```text
SemanticDB plugin belongs to build/scalac, not this LS.
PC plugin belongs to this LS, but cannot write persistent index.
```

最关键的性能原则：

```text
Do not approximate semantic truth.
Precompute exact truth into mmap-friendly structures.
```

最关键的工程原则：

```text
No Nix flake, no build.
No mill-ivy-fetcher lock, no dependency update.
No Java 25, no runtime support.
```
