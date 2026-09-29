/* 切片 S3 测试：HTTP 页面——GET / 返回配置页表单
 * 宿主环境起真实 socket 服务，curl 请求验证 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
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

/* 服务端线程：监听随机端口，处理单个请求后退出 */
static int server_port;
static pthread_t server_tid;

static void *server_main(void *arg) {
    int lfd = *(int *)arg, cfd;
    char buf[8192];
    ssize_t n;
    struct sockaddr_in client;
    socklen_t len = sizeof(client);

    (void)pthread_detach(pthread_self());
    cfd = accept(lfd, (struct sockaddr *)&client, &len);
    if (cfd < 0) return NULL;
    n = recv(cfd, buf, sizeof(buf) - 1, 0);
    if (n > 0) buf[n] = '\0';
    handle_http_request(cfd, buf);   /* 被测函数：解析请求并回写响应 */
    close(cfd);
    return NULL;
}

int main(void) {
    struct sockaddr_in addr;
    int lfd;
    char cmd[256], resp[65536];
    FILE *f;

    lfd = socket(AF_INET, SOCK_STREAM, 0);
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = 0;   /* 随机端口 */
    bind(lfd, (struct sockaddr *)&addr, sizeof(addr));
    {
        socklen_t alen = sizeof(addr);
        getsockname(lfd, (struct sockaddr *)&addr, &alen);
        server_port = ntohs(addr.sin_port);
    }
    listen(lfd, 4);
    pthread_create(&server_tid, NULL, server_main, &lfd);

    /* 客户端请求 GET / */
    snprintf(cmd, sizeof(cmd),
             "curl -s -o " TMPDIR_STR "/s3_resp.txt -w '%%{http_code}' "
             "http://127.0.0.1:%d/", server_port);
    f = popen(cmd, "r");
    CHECK(f != NULL, "curl 启动");
    if (f) {
        char code[8] = {0};
        if (fgets(code, sizeof(code), f)) {
            CHECK(strcmp(code, "200") == 0, "GET / 返回 200");
        } else {
            CHECK(0, "读到 HTTP 状态码");
        }
        pclose(f);
    }

    /* 验证响应体是配置页表单 */
    f = fopen(TMPDIR_STR "/s3_resp.txt", "r");
    CHECK(f != NULL, "响应体文件存在");
    if (f) {
        size_t n = fread(resp, 1, sizeof(resp) - 1, f);
        resp[n] = '\0';
        fclose(f);
        CHECK(strstr(resp, "<form") != NULL, "响应含 <form>");
        CHECK(strstr(resp, "ssid") != NULL, "响应含 ssid 字段");
        CHECK(strstr(resp, "pass") != NULL, "响应含 pass 字段");
        CHECK(strstr(resp, "method=\"post\"") != NULL, "表单 POST");
    }

    close(lfd);
    pthread_join(server_tid, NULL);
    unlink(TMPDIR_STR "/s3_resp.txt");

    printf("\nS3: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}