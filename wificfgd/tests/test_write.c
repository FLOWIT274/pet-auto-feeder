/* 切片 S2 测试：配置写入 write_wifi_config */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/stat.h>
#include "../src/wificfgd.h"

static int failures = 0;
static int checks = 0;

#define CHECK(cond, msg) do { \
    checks++; \
    if (!(cond)) { failures++; printf("FAIL: %s (line %d)\n", msg, __LINE__); } \
} while (0)

/* 读文件内容（截断到 buf），返回长度；不存在返回 -1 */
static long read_file(const char *path, char *buf, size_t cap) {
    FILE *f = fopen(path, "r");
    long n;
    if (!f) return -1;
    n = (long)fread(buf, 1, cap - 1, f);
    buf[n] = '\0';
    fclose(f);
    return n;
}

int main(void) {
    char dir[] = TMPDIR_STR "/test_boot_XXXXXX";
    char path[256];
    char buf[128];
    wifi_creds_t c;
    struct stat st;

    if (!mkdtemp(dir)) { printf("FAIL: mkdtemp\n"); return 1; }
    /* 预置一个 wifi.ap 标记（应被删除）+ wifi.sta 占位（应保留） */
    snprintf(path, sizeof(path), "%s/wifi.ap", dir);
    { FILE *f = fopen(path, "w"); fputs("x", f); fclose(f); }
    snprintf(path, sizeof(path), "%s/wifi.sta", dir);
    { FILE *f = fopen(path, "w"); fputs("old", f); fclose(f); }

    /* 正常写入：内容正确 + 标记切换 */
    strcpy(c.ssid, "HomeNet");
    strcpy(c.pass, "secret123");
    CHECK(write_wifi_config(dir, &c) == 0, "写入成功");

    snprintf(path, sizeof(path), "%s/wifi.ssid", dir);
    CHECK(read_file(path, buf, sizeof(buf)) == 7 && strcmp(buf, "HomeNet") == 0, "wifi.ssid 内容");
    snprintf(path, sizeof(path), "%s/wifi.pass", dir);
    CHECK(read_file(path, buf, sizeof(buf)) == 9 && strcmp(buf, "secret123") == 0, "wifi.pass 内容");
    snprintf(path, sizeof(path), "%s/wifi.sta", dir);
    CHECK(stat(path, &st) == 0, "wifi.sta 已置位");
    snprintf(path, sizeof(path), "%s/wifi.ap", dir);
    CHECK(stat(path, &st) != 0, "wifi.ap 已删除");

    /* 中文 SSID 内容校验 */
    strcpy(c.ssid, "中文");
    CHECK(write_wifi_config(dir, &c) == 0, "中文写入成功");
    snprintf(path, sizeof(path), "%s/wifi.ssid", dir);
    CHECK(read_file(path, buf, sizeof(buf)) == 6 && memcmp(buf, "\xE4\xB8\xAD\xE6\x96\x87", 6) == 0,
           "中文 SSID 原字节写入");

    /* 目录不存在 → 失败 */
    strcpy(c.ssid, "X");
    CHECK(write_wifi_config("/nonexistent_dir_xyz", &c) != 0, "目录不存在时失败");

    /* 清理 */
    snprintf(path, sizeof(path), "%s/wifi.ssid", dir); unlink(path);
    snprintf(path, sizeof(path), "%s/wifi.pass", dir); unlink(path);
    snprintf(path, sizeof(path), "%s/wifi.sta", dir); unlink(path);
    rmdir(dir);

    printf("\nS2: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}