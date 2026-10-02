# 后端目录与验证

`src/` 保存业务实现，`tests/unit/` 集中保存单元测试主体，
`tests/fixtures/` 保存测试产物的源码。

| 目录 | 职责 |
| --- | --- |
| `src/api/` | HTTP 路由、请求模型、中间件、处理器与共享状态 |
| `src/auth/` | 登录与认证 |
| `src/core/app/` | 配置、日志、错误与应用基础服务 |
| `src/core/audio/` | 音频探测、转码与流式处理 |
| `src/core/books/` | 书籍元数据、NFO 与刮削业务 |
| `src/core/library_scanner/` | 本地、WebDAV、RSS 扫描与调度 |
| `src/core/security/` | 加密、签名与密钥管理 |
| `src/core/storage/` | 文件路径及存储访问 |
| `src/core/task_queue/` | 后台任务与执行管理 |
| `src/db/` | 数据库模型、迁移与仓储 |
| `src/plugin/` | 插件安装、能力授权、宿主接口与三类运行时 |
| `tests/unit/` | 按业务模块对应的单元测试 |
| `tests/fixtures/` | 原生 ABI 等测试夹具 |

独立且长度适中的完整业务使用单文件，例如 `nfo_manager.rs`、
`streamer.rs`、`sandbox.rs`；只有多个独立职责需要分离时才使用目录模块。
业务文件通过 `#[cfg(test)]` 和 `#[path = "..."]` 引用测试文件，
测试仍作为原模块的子模块编译，能够验证私有实现。

常规检查：

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test --lib -- --test-threads=1
```

依赖外部 WASM、原生库或签名包的集成测试需要先生成对应产物并设置
测试声明中的 `TING_TEST_*` 环境变量，再使用 `--ignored` 显式运行。
默认忽略的测试不计入已经通过的测试数量。
