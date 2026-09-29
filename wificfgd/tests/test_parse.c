/* 切片 S1 测试：表单解析 parse_form */
#include <stdio.h>
#include <string.h>
#include "../src/wificfgd.h"

static int failures = 0;
static int checks = 0;

#define CHECK(cond, msg) do { \
    checks++; \
    if (!(cond)) { failures++; printf("FAIL: %s (line %d)\n", msg, __LINE__); } \
} while (0)

/* 期望解析成功且字段精确匹配 */
static void expect_ok(const char *body, const char *want_ssid, const char *want_pass) {
    wifi_creds_t c;
    int rc = parse_form(body, &c);
    checks++;
    if (rc != 0) { failures++; printf("FAIL: body='%s' rc=%d 期望 0\n", body, rc); return; }
    if (strcmp(c.ssid, want_ssid) != 0) { failures++; printf("FAIL: ssid='%s' 期望 '%s'\n", c.ssid, want_ssid); }
    if (strcmp(c.pass, want_pass) != 0) { failures++; printf("FAIL: pass='%s' 期望 '%s'\n", c.pass, want_pass); }
    checks += 2;
}

/* 期望解析失败并返回指定错误码 */
static void expect_err(const char *body, int want_rc) {
    wifi_creds_t c;
    int rc = parse_form(body, &c);
    checks++;
    if (rc != want_rc) { failures++; printf("FAIL: body='%s' rc=%d 期望 %d\n", body, rc, want_rc); }
}

int main(void) {
    /* 基本解析 */
    expect_ok("ssid=MyWiFi&pass=secret123", "MyWiFi", "secret123");
    /* 加号 = 空格 */
    expect_ok("ssid=My+WiFi&pass=secret123", "My WiFi", "secret123");
    /* 百分号解码（UTF-8 中文 SSID） */
    expect_ok("ssid=%E4%B8%AD%E6%96%87&pass=abc12345", "中文", "abc12345");
    /* 字段顺序可交换 */
    expect_ok("pass=p%40ssword&ssid=HomeNet", "HomeNet", "p@ssword");
    /* 空字段 */
    expect_err("ssid=&pass=abc12345", -2);
    expect_err("ssid=MyWiFi&pass=", -2);
    /* 缺字段 */
    expect_err("ssid=MyWiFi", -1);
    expect_err("pass=abc12345", -1);
    /* 超长 */
    {
        char body[200], longstr[80];
        memset(longstr, 'A', sizeof(longstr) - 1);
        longstr[sizeof(longstr) - 1] = '\0';
        snprintf(body, sizeof(body), "ssid=%.*s&pass=abc12345", SSID_MAX + 5, longstr);
        expect_err(body, -3);
        snprintf(body, sizeof(body), "ssid=MyWiFi&pass=%.*s", PASS_MAX + 5, longstr);
        expect_err(body, -4);
    }
    /* 空 body */
    expect_err("", -1);

    printf("\nS1: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}