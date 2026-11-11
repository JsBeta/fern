# Release notes

`release.yml` 在创建 Release 时读取 `docs/release-notes/<tag>.md` 作为正文（tag 形如 `v0.1.3`）。

- 有对应文件 → `gh release create --notes-file`；如果该 Release 已存在（重跑、手动触发），改用 `gh release edit --notes-file` 覆盖正文，再上传产物。
- 没有对应文件 → 退回 `--generate-notes`（只会得到一条 Full Changelog 链接，因为本项目直接推 `main`，没有 PR）。

所以发版的顺序是：先在 `Cargo.toml` 提版本号，再提交 `docs/release-notes/v<版本>.md`，然后打 tag 推送。**正文必须和 tag 一起进仓库**，发布后就不再需要手工用 API 补写。

文件内容就是 GitHub Release 正文，支持 Markdown，不需要额外的前置元数据。习惯的结构：

```markdown
## 本次更新

### 新增：<一句话主题>

- 具体变化，写清楚行为变化与触发条件。

### 优化：<一句话主题>

- 具体变化。

### 修复：<一句话主题>

- 具体变化。

### 升级说明

直接替换可执行文件即可，配置文件与数据目录结构没有变化。

### 校验

下载后可用附件中的 `SHA256SUMS` 校验文件完整性：

```sh
sha256sum -c SHA256SUMS
```

## 完整变更

https://github.com/JsBeta/fern/compare/<上一个 tag>...<本 tag>
```

草稿发布（`workflow_dispatch` 勾选 `draft`）同样会写入正文，检查无误后在页面上点发布即可。
