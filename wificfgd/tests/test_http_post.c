/* 切片 S4 测试：POST /configure 全流程
 * 真实 socket + curl POST → 200 + 凭证落盘到注入目录 + 重启命令被调用 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <pthread.h>
#include "../src/wificfgd.h"

static int failures = 0;
static int checks = 0;

#define CHECK(cond, msg) do { \
    checks++; \
    if (!(cond)) { failures++; printf("FAIL: %s (line %d)\n", msg, __LINE__); } \
} while (0)

static int server_port;
static pthread_t server_tid;

static void *server_main(void *arg) {
    int lfd = *(int *)arg, cfd;
    char buf[16384];
    ssize_t n;
    struct sockaddr_in client;
    socklen_t len = sizeof(client);

    cfd = accept(lfd, (struct sockaddr *)&client, &len);
    if (cfd < 0) return NULL;
    n = recv(cfd, buf, sizeof(buf) - 1, 0);
    if (n > 0) buf[n] = '\0';
    handle_http_request(cfd, buf);
    close(cfd);
    return NULL;
}

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
    struct sockaddr_in addr;
    int lfd;
    char cmd[512], buf[128], path[256], tmpdir[] = TMPDIR_STR "/s4_boot_XXXXXX";
    FILE *f;
    struct stat st;

    if (!mkdtemp(tmpdir)) { printf("FAIL: mkdtemp\n"); return 1; }

    /* 注入配置目录 + 假 reboot 命令（记日志不真重启） */
    wificfgd_boot_dir = tmpdir;
    setenv("WIFICFGD_REBOOT_CMD", TMPDIR_STR "/fake_reboot.sh", 1);
    {
        FILE *r = fopen(TMPDIR_STR "/fake_reboot.sh", "w");
        fputs("#!/bin/sh\necho reboot >> " TMPDIR_STR "/s4_reboot.log\n", r);
        fclose(r);
        chmod(TMPDIR_STR "/fake_reboot.sh", 0755);
    }

    lfd = socket(AF_INET, SOCK_STREAM, 0);
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = 0;
    bind(lfd, (struct sockaddr *)&addr, sizeof(addr));
    {
        socklen_t alen = sizeof(addr);
        getsockname(lfd, (struct sockaddr *)&addr, &alen);
        server_port = ntohs(addr.sin_port);
    }
    listen(lfd, 4);
    pthread_create(&server_tid, NULL, server_main, &lfd);

    /* POST 提交表单 */
    snprintf(cmd, sizeof(cmd),
             "curl -s -o " TMPDIR_STR "/s4_resp.txt -w '%%{http_code}' "
             "-d 'ssid=HomeNet%%2B5G&pass=secret%%40123' http://127.0.0.1:%d/configure",
             server_port);
    f = popen(cmd, "r");
    if (f) {
        char code[8] = {0};
        if (fgets(code, sizeof(code), f)) CHECK(strcmp(code, "200") == 0, "POST 返回 200");
        else CHECK(0, "读到状态码");
        pclose(f);
    } else CHECK(0, "curl 启动");

    /* 凭证落盘（urlencoded 已解码） */
    snprintf(path, sizeof(path), "%s/wifi.ssid", tmpdir);
    CHECK(read_file(path, buf, sizeof(buf)) == 10 && strcmp(buf, "HomeNet+5G") == 0,
           "wifi.ssid 解码落盘");
    snprintf(path, sizeof(path), "%s/wifi.pass", tmpdir);
    CHECK(read_file(path, buf, sizeof(buf)) == 10 && strcmp(buf, "secret@123") == 0,
           "wifi.pass 解码落盘");
    snprintf(path, sizeof(path), "%s/wifi.sta", tmpdir);
    CHECK(stat(path, &st) == 0, "wifi.sta 已置位");

    /* 重启命令被调用 */
    sleep(1);
    snprintf(path, sizeof(path), "%s", TMPDIR_STR "/s4_reboot.log");
    CHECK(access(path, F_OK) == 0, "reboot 命令已调用");

    /* 响应体为确认页 */
    f = fopen(TMPDIR_STR "/s4_resp.txt", "r");
    if (f) {
        size_t n = fread(buf, 1, sizeof(buf) - 1, f);
        buf[n] = '\0';
        fclose(f);
        CHECK(strstr(buf, "重启") != NULL || strstr(buf, "reboot") != NULL, "确认页含重启提示");
    } else CHECK(0, "响应体存在");

    close(lfd);
    pthread_join(server_tid, NULL);

    /* 清理 */
    snprintf(path, sizeof(path), "%s/wifi.ssid", tmpdir); unlink(path);
    snprintf(path, sizeof(path), "%s/wifi.pass", tmpdir); unlink(path);
    snprintf(path, sizeof(path), "%s/wifi.sta", tmpdir); unlink(path);
    rmdir(tmpdir);
    unlink(TMPDIR_STR "/fake_reboot.sh");
    unlink(TMPDIR_STR "/s4_reboot.log");
    unlink(TMPDIR_STR "/s4_resp.txt");

    printf("\nS4: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}