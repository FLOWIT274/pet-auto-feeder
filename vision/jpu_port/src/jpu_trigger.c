// jpu_trigger.c — 通过驱动内置测试钩子触发 JPU 硬解
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <string.h>
#include <sys/ioctl.h>
#define CVI_VC_DRV_IOCTL_MAGIC 'S'
#define CVI_VC_DRV_IOCTL_MAGIC 'V'
#define CVI_VC_ENC_DEC_JPEG_TEST _IO('V', 67)

int main(int argc, char **argv)
{
    const char *dev = (argc > 1) ? argv[1] : "/dev/cvi_vc_dec0";
    int fd = open(dev, O_RDWR);
    if (fd < 0) { perror(dev); return 1; }
    char cmd[512] = {0};
    snprintf(cmd, sizeof(cmd), "%s", argv[2]);
    printf("ioctl %s <- \"%s\"\n", dev, cmd);
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    int ret = ioctl(fd, CVI_VC_ENC_DEC_JPEG_TEST, cmd);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    double ms = (t1.tv_sec-t0.tv_sec)*1000.0 + (t1.tv_nsec-t0.tv_nsec)/1e6;
    printf("耗时=%.1fms\n", ms);
    printf("rc=%d errno=%d\n", ret, 0);
    close(fd);
    return ret;
}
