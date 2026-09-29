#ifndef WIFICFGD_H
#define WIFICFGD_H

/* 公开接口：wificfgd 各模块 */
#define SSID_MAX 32      /* IEEE 802.11 SSID 上限（字节） */
#define PASS_MAX 63      /* WPA2 密码上限（字节） */

typedef struct {
    char ssid[SSID_MAX + 1];  /* 均以 NUL 结尾 */
    char pass[PASS_MAX + 1];
} wifi_creds_t;

/* S1: 解析 urlencoded 表单 body（ssid=..&pass=..），百分号+加号解码。
 * 返回 0 成功；负值为错误码：
 *   -1 缺字段  -2 字段为空  -3 ssid 超长  -4 pass 超长 */
int parse_form(const char *body, wifi_creds_t *out);

/* S2: 把凭证写入配置目录（/boot）：
 *   - 写 wifi.ssid、wifi.pass
 *   - 置 wifi.sta 标记（touch）
 *   - 删 wifi.ap 标记
 * dir 注入以便测试；返回 0 成功，负值为错误码（-1 写入失败）。 */
int write_wifi_config(const char *dir, const wifi_creds_t *creds);

/* S3/S4: 处理单个已接受的 HTTP 连接（解析请求 → 路由 → 回写响应） */
void handle_http_request(int client_fd, const char *request);

/* 配置目录（/boot），全局注入以便测试 */
extern const char *wificfgd_boot_dir;

#endif