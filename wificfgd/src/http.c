/* HTTP 服务：极简单连接处理——解析请求行，路由 GET / 与 POST /configure */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include "wificfgd.h"

const char *wificfgd_boot_dir = "/boot";

/* 回写完整 HTTP 响应 */
static void send_response(int fd, int status, const char *status_text,
                          const char *content_type, const char *body) {
    char head[512];
    int n = snprintf(head, sizeof(head),
        "HTTP/1.1 %d %s\r\n"
        "Content-Type: %s\r\n"
        "Content-Length: %zu\r\n"
        "Connection: close\r\n"
        "\r\n",
        status, status_text, content_type, strlen(body));
    (void)send(fd, head, (size_t)n, 0);
    (void)send(fd, body, strlen(body), 0);
}

/* 配置页 HTML：表单 POST 到 /configure */
static void serve_config_page(int fd) {
    const char *html =
        "<!DOCTYPE html>\n"
        "<html><head><meta charset=\"utf-8\">"
        "<title>WiFi 配置</title></head><body>\n"
        "<h1>WiFi 配置</h1>\n"
        "<p>请输入要连接的 WiFi 名称和密码：</p>\n"
        "<form method=\"post\" action=\"/configure\">\n"
        "  SSID: <input type=\"text\" name=\"ssid\" maxlength=\"32\" required><br>\n"
        "  密码: <input type=\"password\" name=\"pass\" maxlength=\"63\" required><br>\n"
        "  <input type=\"submit\" value=\"保存并重启\">\n"
        "</form>\n"
        "</body></html>\n";
    send_response(fd, 200, "OK", "text/html; charset=utf-8", html);
}

/* 提交处理：解析 body → 校验 → 落盘 → 确认页 + 触发重启 */
static void serve_configure(int fd, const char *request) {
    /* 找请求体（\r\n\r\n 之后）；本服务只接受单请求无 keep-alive，body 随首个 recv 到达 */
    const char *body = strstr(request, "\r\n\r\n");
    wifi_creds_t creds;
    char html[512];
    int rc;

    if (!body) { send_response(fd, 400, "Bad Request", "text/plain", "missing body\n"); return; }
    body += 4;

    rc = parse_form(body, &creds);
    if (rc != 0) {
        snprintf(html, sizeof(html),
            "<!DOCTYPE html><html><body><h1>配置无效</h1>"
            "<p>错误码 %d。SSID 1-32 字符，密码 1-63 字符。</p>"
            "<p><a href=\"/\">返回重试</a></p></body></html>\n", rc);
        send_response(fd, 400, "Bad Request", "text/html; charset=utf-8", html);
        return;
    }

    rc = write_wifi_config(wificfgd_boot_dir, &creds);
    if (rc != 0) {
        send_response(fd, 500, "Internal Server Error", "text/plain", "write failed\n");
        return;
    }

    /* 触发重启：先 sync 确保凭证落盘；命令可注入（测试用假命令），默认 /sbin/reboot */
    (void)sync();
    {
        const char *reboot_cmd = getenv("WIFICFGD_REBOOT_CMD");
        if (!reboot_cmd) reboot_cmd = "/sbin/reboot";
        (void)system(reboot_cmd);
    }

    snprintf(html, sizeof(html),
        "<!DOCTYPE html><html><body><h1>已保存</h1>"
        "<p>配置已保存，设备正在重启，请稍后重新连接 WiFi「%s」。</p>"
        "</body></html>\n", creds.ssid);
    send_response(fd, 200, "OK", "text/html; charset=utf-8", html);
}

void handle_http_request(int client_fd, const char *request) {
    char method[8], path[128];
    int matched;

    if (!request) return;
    matched = sscanf(request, "%7s %127s HTTP/", method, path);
    if (matched != 2) { send_response(client_fd, 400, "Bad Request", "text/plain", "bad request\n"); return; }

    if (strcmp(method, "GET") == 0 && strcmp(path, "/") == 0) {
        serve_config_page(client_fd);
        return;
    }
    if (strcmp(method, "POST") == 0 && strcmp(path, "/configure") == 0) {
        serve_configure(client_fd, request);
        return;
    }
    if (strcmp(method, "GET") == 0 && strcmp(path, "/favicon.ico") == 0) {
        send_response(client_fd, 404, "Not Found", "text/plain", "");
        return;
    }
    send_response(client_fd, 404, "Not Found", "text/plain", "not found\n");
}