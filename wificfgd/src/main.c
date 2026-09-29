/* wificfgd 主程序：监听 80 端口，逐连接处理 HTTP 请求 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include "wificfgd.h"

int main(int argc, char *argv[]) {
    int lfd, cfd, opt = 1;
    struct sockaddr_in addr;
    unsigned short port = 80;

    /* 配置目录可注入（测试/调试用），默认 /boot */
    {
        const char *bd = getenv("WIFICFGD_BOOT_DIR");
        if (bd && *bd) wificfgd_boot_dir = bd;
    }

    /* -p <port> 覆盖端口（测试/调试用） */
    if (argc >= 3 && strcmp(argv[1], "-p") == 0) {
        port = (unsigned short)atoi(argv[2]);
    }

    /* 忽略 SIGPIPE：客户端断开时 write 不杀进程 */
    signal(SIGPIPE, SIG_IGN);

    lfd = socket(AF_INET, SOCK_STREAM, 0);
    if (lfd < 0) { perror("socket"); return 1; }
    setsockopt(lfd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port = htons(port);
    if (bind(lfd, (struct sockaddr *)&addr, sizeof(addr)) < 0) { perror("bind"); return 1; }
    if (listen(lfd, 4) < 0) { perror("listen"); return 1; }

    for (;;) {
        cfd = accept(lfd, NULL, NULL);
        if (cfd < 0) { perror("accept"); continue; }
        {
            char buf[16384];
            ssize_t n = recv(cfd, buf, sizeof(buf) - 1, 0);
            if (n > 0) {
                buf[n] = '\0';
                handle_http_request(cfd, buf);
            }
        }
        close(cfd);
    }
}