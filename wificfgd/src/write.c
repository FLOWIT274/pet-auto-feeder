/* write_wifi_config：凭证落盘 + 标记切换（置 sta、删 ap） */
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include "wificfgd.h"

/* 原子写文件：写临时文件后 rename，避免中断留下半截内容 */
static int write_file_atomic(const char *dir, const char *name, const char *data) {
    char tmp[256], final[256];
    FILE *f;
    int ok = 1;

    snprintf(tmp, sizeof(tmp), "%s/.%s.tmp", dir, name);
    snprintf(final, sizeof(final), "%s/%s", dir, name);
    f = fopen(tmp, "w");
    if (!f) return -1;
    if (fwrite(data, 1, strlen(data), f) != strlen(data)) ok = 0;
    if (fclose(f) != 0) ok = 0;
    if (ok && rename(tmp, final) != 0) ok = 0;
    if (!ok) { unlink(tmp); return -1; }
    return 0;
}

int write_wifi_config(const char *dir, const wifi_creds_t *creds) {
    char sta_path[256];
    if (!dir || !creds) return -1;

    if (write_file_atomic(dir, "wifi.ssid", creds->ssid) != 0) return -1;
    if (write_file_atomic(dir, "wifi.pass", creds->pass) != 0) return -1;

    /* 置 wifi.sta 标记（已存在则不变） */
    snprintf(sta_path, sizeof(sta_path), "%s/wifi.sta", dir);
    if (access(sta_path, F_OK) != 0) {
        FILE *f = fopen(sta_path, "w");
        if (!f) return -1;
        fclose(f);
    }

    /* 删 wifi.ap 标记 */
    snprintf(sta_path, sizeof(sta_path), "%s/wifi.ap", dir);
    if (access(sta_path, F_OK) == 0 && unlink(sta_path) != 0) return -1;

    return 0;
}