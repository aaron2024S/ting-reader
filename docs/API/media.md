# 媒体流

## 音频流

### GET /api/stream/:chapterId

流式播放章节音频。支持 Range 请求和转码。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| chapterId | string | 章节 ID |

**查询参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| token | string | 认证 Token（可选） |
| transcode | string | 转码格式：`mp3`、`wav`、`hls`（可选） |
| seek | string | 跳转位置，如 `30.5`（秒，可选，仅转码模式） |
| download | string | 下载模式标识：`1` / `true` / `yes` / `on`（可选）。该标志会透传给格式插件，供插件区分在线播放与离线下载场景 |

**请求头：**

| Header | 说明 |
|--------|------|
| `Range` | 标准 HTTP Range 头，如 `bytes=0-1023` |

**响应头：**

| Header | 说明 |
|--------|------|
| `Content-Type` | 音频 MIME 类型 |
| `Content-Length` | 内容长度 |
| `Content-Range` | Range 响应 |
| `Accept-Ranges` | `bytes` |
| `X-Audio-Duration` | 音频时长（秒，转码模式） |
| `X-Download-Extension` | 建议的文件扩展名（如 `mp3`、`m4a`、`flac`，仅插件处理格式时返回） |

**处理优先级：**

1. `.strm` 文件 → URL 重定向；指定 `transcode` 时走转码
2. HLS 转码（`transcode=hls`）→ 返回 HLS 会话信息
3. 普通转码（`transcode=mp3/wav`）→ FFmpeg 管道转码
4. 内存预加载缓存 → 返回已缓存的前缀，后续内容接续源文件流
5. 磁盘缓存 → 直接返回
6. 扩展格式插件处理 → 按统一播放能力返回
7. 本地/WebDAV/HTTP 直接流式传输

**支持格式：** m4a, mp4, mp3, aac, flac, ogg, opus, wav, wma, strm

**自动预加载：**

开启 `auto_preload` 后，服务端预读下一章的开头，单章窗口为 4 MiB。超过 4 MiB 的章节也会预加载；播放时先发送缓存前缀，再流式读取剩余内容。请求范围全部位于前缀内时直接使用内存，跨越窗口或位于窗口之后时按实际偏移读取源文件。

响应的 `Content-Length` 和 `Content-Range` 使用章节总长度。源站未提供总长度时，完整播放使用不声明长度的流式响应；无法确定的 Range 请求回到源文件处理，仍无法确定长度时返回 `200` 完整流。源站忽略 Range 时，服务端流式跳过已缓存的前缀，保证音频字节连续，但这一回退会重新下载被跳过的部分。

内存缓存总上限为 32 MiB，空闲 60 秒后到期，定时清理间隔为 30 秒，最多同时执行 4 个预加载任务。章节切换或关闭设置会取消对应任务。`.strm` 不触发服务端预读，需要格式插件流式处理的章节走插件播放链路。`auto_cache` 使用独立的磁盘缓存预算，完整文件始终流式写入和读取。

**转码说明：**
- `transcode=mp3`：通过 FFmpeg 转码为 MP3（128kbps）
- `transcode=wav`：通过 FFmpeg 转码为 WAV
- `transcode=hls`：创建 HLS 转码会话，返回播放列表地址（见下方 HLS 章节）

**HLS 初始化响应：**

当请求 `/api/stream/:chapterId?transcode=hls` 时，响应为 JSON：

```json
{
  "type": "hls",
  "session_id": "string",
  "playlist_url": "/api/stream/hls/{sessionId}/playlist.m3u8",
  "is_strm": false,
  "ready": true
}
```

可通过 `seek` 查询参数指定初始位置，例如 `/api/stream/:chapterId?transcode=hls&seek=120.5`。

**STRM 文件：** 读取文件中的 HTTP(S) 音源地址，普通播放返回 `302`，由客户端直接连接音源。地址中的用户名、密码、签名和查询参数保持原样，服务端不因此下载或代理音频。请求 `transcode=mp3/wav/hls` 时，服务端读取音源并转码。

Web 使用普通音频元素加载重定向地址，不设置 `crossOrigin`。查询参数形式的认证链接可以沿用此链路；`user:password@host` 形式的地址能否播放取决于客户端支持，在 Chrome 的跨域重定向测试中被拦截。客户端加载失败不改变普通 STRM 接口的直连行为；Flutter 保留首轮加载失败后请求 MP3 转码的恢复策略。

---

### GET /api/v1/public/media/:chapterId（签名公开流）

无需登录的公开音频流接口，用于分享场景。URL 需携带签名参数，签名由服务端通过 `POST /api/v1/plugin-route-signatures` 生成。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| chapterId | string | 章节 ID |

**查询参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| expires | number | 签名过期时间戳（Unix 秒） |
| user | string | 用户 ID（签名绑定） |
| signature | string | HMAC 签名 |
| transcode | string | 转码格式：`mp3`、`wav`、`hls`（可选） |
| seek | string | 跳转位置（秒，可选） |
| download | string | 下载模式标识（可选） |

**说明：**
- 签名过期或校验失败返回 `403`
- 签名绑定的用户身份用于权限校验（检查该用户是否有权访问对应书籍）
- 同时支持 `/api/public/media/:chapterId`（无 v1 前缀）

---

## HLS 流

### GET /api/stream/hls/:sessionId/playlist.m3u8

获取 HLS 播放列表（无需认证，Session ID 提供安全保护）。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| sessionId | string | HLS 会话 ID |

---

### GET /api/stream/hls/:sessionId/:filename

获取 HLS 分片（无需认证）。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| sessionId | string | HLS 会话 ID |
| filename | string | 分片文件名，如 `segment_000.ts` |

---

### POST /api/stream/hls/:sessionId/seek

HLS 流跳转（无需认证）。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| sessionId | string | HLS 会话 ID |

**查询参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| seek | number | 跳转秒数 |

**响应：** `200 OK`

```json
{
  "status": "seeked",
  "seek_time": 120.5,
  "seq": 2,
  "playlist_url": "/api/stream/hls/{sessionId}/playlist.m3u8?seq=2"
}
```

---

## 封面代理

### GET /api/proxy/cover

代理封面图片，支持本地文件、外部 URL 和 WebDAV。

**查询参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| path | string | 图片路径或 URL |
| library_id | string | 媒体库 ID（可选） |
| book_id | string | 书籍 ID（可选） |

**特殊路径：**
- `embedded://first-chapter`：从音频文件提取封面（暂未实现）
- `http://...#referer=XXX`：带 Referer 的外部图片

**响应：** 图片二进制数据，带 `Cache-Control: public, max-age=31536000`

---

## 缓存管理

### GET /api/cache

获取缓存列表（管理员）。

**响应：** `200 OK`

```json
{
  "caches": [
    {
      "chapter_id": "string",
      "book_id": "string | null",
      "book_title": "string | null",
      "chapter_title": "string | null",
      "file_size": 0,
      "created_at": "RFC3339 | null",
      "cover_url": "string | null"
    }
  ],
  "total": 0,
  "total_size": 0
}
```

---

### POST /api/cache/:chapterId

缓存章节到本地（管理员，用于 WebDAV 远程文件）。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| chapterId | string | 章节 ID |

**响应：** `200 OK`

```json
{
  "success": true,
  "message": "Chapter xxx cached successfully",
  "cache_info": {
    "chapter_id": "string",
    "book_id": "string",
    "book_title": "string",
    "chapter_title": "string",
    "file_size": 0,
    "created_at": "RFC3339",
    "cover_url": "string"
  }
}
```

---

### DELETE /api/cache/:chapterId

删除指定章节缓存（管理员）。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| chapterId | string | 章节 ID |

**响应：** `200 OK`

```json
{
  "success": true,
  "message": "Cache for chapter xxx deleted successfully"
}
```

---

### DELETE /api/cache

清除所有缓存（管理员）。

**响应：** `200 OK`

```json
{
  "success": true,
  "deleted_count": 0,
  "message": "Cleared 0 cached chapters"
}
```
