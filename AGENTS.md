# RepoPilot · AGENTS.md

Tauri 2 + Vue 3 + TypeScript 的本地多仓库 Git 批量管理桌面工具（中文界面）。

## 开发命令

- 前端构建 / 类型检查：`npm run build`（含 vue-tsc）
- 开发运行：`npm run tauri dev`（dev 日志 `/tmp/repopilot_dev.log`，端口 1420）
- Rust 改动后必须 `touch src-tauri/src/lib.rs` 强制重编
- 重启 dev：pkill tauri dev / vite / repo-pilot 三进程后再启动

## 硬性约定

- **新增任何用户可见功能后，必须同步更新 `README.md` 的「功能」清单**（用户明确要求，每次迭代都要遵守）
- 界面文案走 `src/App.vue` 内 i18n（zh / en 两套），新增文案必须同时补两套
- 新功能 / 方向先记入 `ROADMAP.md`，实现后从"待实现"移到功能清单
- 界面与交互按用户实测反馈小步迭代，改动要具体可验证
