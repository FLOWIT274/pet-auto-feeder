/* parse_form：解析 urlencoded 表单 body（ssid=..&pass=..） */
#include <string.h>
#include <stdlib.h>
#include <ctype.h>
#include "wificfgd.h"

/* 解码 urlencoded 值：%XX 转字节、+ 转空格。返回解码后长度（不超 src 长度）。
 * 解码只会缩短或持平，故 src 长度 >= 解码长度；src 塞不下 dst 即必然溢出。 */
static int url_decode(const char *src, char *dst, size_t dst_cap) {
    size_t i = 0, o = 0;
    if (strlen(src) + 1 > dst_cap) return -1;   /* 必然截断，拒绝 */
    while (src[i] && o + 1 < dst_cap) {
        if (src[i] == '%' && isxdigit((unsigned char)src[i + 1]) &&
            isxdigit((unsigned char)src[i + 2])) {
            int hi = src[i + 1] <= '9' ? src[i + 1] - '0' : (tolower(src[i + 1]) - 'a' + 10);
            int lo = src[i + 2] <= '9' ? src[i + 2] - '0' : (tolower(src[i + 2]) - 'a' + 10);
            dst[o++] = (char)(hi * 16 + lo);
            i += 3;
        } else if (src[i] == '+') {
            dst[o++] = ' ';
            i++;
        } else {
            dst[o++] = src[i];
            i++;
        }
    }
    dst[o] = '\0';
    return (int)o;
}

int parse_form(const char *body, wifi_creds_t *out) {
    const char *p = body;
    int have_ssid = 0, have_pass = 0;
    char decoded[64];

    if (!body || !out) return -1;

    while (*p) {
        /* 提取单个 field：name=value，以 & 分隔 */
        const char *amp = strchr(p, '&');
        size_t field_len = amp ? (size_t)(amp - p) : strlen(p);
        const char *eq = memchr(p, '=', field_len);
        if (eq) {
            size_t name_len = (size_t)(eq - p);
            const char *value = eq + 1;
            size_t value_len = field_len - name_len - 1;
            char value_buf[128];
            int dlen;

            if (value_len >= sizeof(value_buf)) value_len = sizeof(value_buf) - 1;
            memcpy(value_buf, value, value_len);
            value_buf[value_len] = '\0';
            dlen = url_decode(value_buf, decoded, sizeof(decoded));

            if (name_len == 4 && memcmp(p, "ssid", 4) == 0) {
                if (dlen < 0 || dlen > SSID_MAX) return -3;   /* 超缓冲或超长 */
                if (dlen == 0) return -2;               /* 空 ssid */
                memcpy(out->ssid, decoded, (size_t)dlen + 1);
                have_ssid = 1;
            } else if (name_len == 4 && memcmp(p, "pass", 4) == 0) {
                if (dlen < 0 || dlen > PASS_MAX) return -4;   /* 超缓冲或超长 */
                if (dlen == 0) return -2;               /* 空 pass */
                memcpy(out->pass, decoded, (size_t)dlen + 1);
                have_pass = 1;
            }
        }
        if (!amp) break;
        p = amp + 1;
    }

    if (!have_ssid || !have_pass) return -1;            /* 缺字段 */
    return 0;
}