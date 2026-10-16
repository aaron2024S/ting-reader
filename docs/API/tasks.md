# 任务管理

所有任务接口仅管理员可访问。

失败或取消任务的 `error` 使用统一的 `error/message/details/trace_id` 对象，正常任务为 `null`。任务历史会保存错误分类和追踪信息；旧记录仅有文本时，`details.reason` 为 `unclassified`，追踪值以 `legacy-task-` 开头，不代表历史请求的追踪 ID。任务仅对可重试错误应用重试策略，参数错误、权限不足和取消不自动重试。

## GET /api/v1/tasks

获取任务列表。

**查询参数：**

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| status | string | - | 过滤状态：queued, running, completed, failed, cancelled |
| page | number | 1 | 页码 |
| page_size | number | 20 | 每页数量 |
| sort_by | string | created_at | 排序字段 |
| sort_order | string | desc | 排序方向：asc, desc |

**响应：** `200 OK`

```json
[
  {
    "id": "string",
    "task_type": "string",
    "status": "queued | running | completed | failed | cancelled",
    "payload": "string (JSON)",
    "message": "string | null",
    "error": null,
    "retries": 0,
    "max_retries": 0,
    "created_at": "RFC3339",
    "started_at": "RFC3339 | null",
    "finished_at": "RFC3339 | null"
  }
]
```

---

## GET /api/v1/tasks/:id

获取任务详情。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| id | string | 任务 ID |

**响应：** `200 OK`

```json
{
  "id": "string",
  "task_type": "string",
  "status": "string",
  "payload": {},
  "message": "string | null",
  "result": {},
  "error": null,
  "retries": 0,
  "max_retries": 0,
  "created_at": "RFC3339",
  "started_at": "RFC3339 | null",
  "finished_at": "RFC3339 | null"
}
```

---

## POST /api/v1/tasks/:id/cancel

取消任务。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| id | string | 任务 ID |

**响应：** `200 OK`

```json
{
  "message": "Task xxx cancelled successfully"
}
```

---

## DELETE /api/v1/tasks/:id

删除任务。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| id | string | 任务 ID |

**响应：** `200 OK`

```json
{
  "message": "Task xxx deleted successfully"
}
```

---

## DELETE /api/v1/tasks

清除任务。

**查询参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| status | string | 按状态过滤（可选） |

**响应：** `200 OK`

```json
{
  "message": "Cleared 5 tasks",
  "count": 5
}
```

---

## POST /api/v1/tasks/batch-delete

批量删除任务。

**请求体：**

```json
{
  "ids": ["task-id-1", "task-id-2"]
}
```

**响应：** `200 OK`

```json
{
  "message": "Deleted 2 tasks",
  "count": 2
}
```
