// vdec_mjpeg_test3.c — PT_MJPEG 硬解验证（专用 VB 池 + AttachVbPool）
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "cvi_vdec.h"
#include "cvi_sys.h"
#include "cvi_vb.h"
#include "cvi_comm_vb.h"

#ifndef ALIGN
#define ALIGN(v, a) (((v) + (a) - 1) & ~((a) - 1))
#endif

int main(int argc, char **argv)
{
    int mjpeg = (argc > 1 && atoi(argv[1]) == 1);
    const char *jpg = (argc > 2) ? argv[2] : "/mnt/data/tpu/cap.jpg";
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("SYS_Init rc=%#x\n", CVI_SYS_Init());

    // ---- VB 框架初始化 ----
    {
        VB_CONFIG_S cconf;
        memset(&cconf, 0, sizeof(cconf));
        cconf.u32MaxPoolCnt = 1;
        cconf.astCommPool[0].u32BlkSize = ALIGN(640 * 480 * 3 / 2, 4096);
        cconf.astCommPool[0].u32BlkCnt = 4;
        int vr = CVI_VB_SetConfig(&cconf);
        printf("CVI_VB_SetConfig rc=%#x\n", vr);
        vr = CVI_VB_Init();
        printf("CVI_VB_Init rc=%#x\n", vr);
    }
    // ---- 专用 VB 池: 640x480 NV12 ----
    VB_POOL_CONFIG_S pcfg;
    memset(&pcfg, 0, sizeof(pcfg));
    pcfg.u32BlkSize = ALIGN(640 * 480 * 3, 16384);
    pcfg.u32BlkCnt = 4;
    strcpy(pcfg.acName, "vdec_jpg");
    VB_POOL pool = CVI_VB_CreatePool(&pcfg);
    printf("CVI_VB_CreatePool rc=%d\n", pool);
    if (pool < 0) return 1;

    VDEC_CHN_ATTR_S attr;
    memset(&attr, 0, sizeof(attr));
    attr.enType = mjpeg ? PT_MJPEG : PT_H264;
    attr.enMode = VIDEO_MODE_FRAME;
    attr.u32PicWidth = 640;
    attr.u32PicHeight = 480;
    attr.u32StreamBufSize = ALIGN(640 * 480, 0x4000);
    attr.u32FrameBufCnt = 3;

    int ret = CVI_VDEC_CreateChn(0, &attr);
    printf("CreateChn(%s) rc=%#x\n", mjpeg ? "PT_MJPEG" : "PT_H264", ret);
    if (ret != 0) return 1;

    VDEC_CHN_PARAM_S p;
    memset(&p, 0, sizeof(p));
    CVI_VDEC_GetChnParam(0, &p);
    p.enPixelFormat = PIXEL_FORMAT_NV12;
    p.u32DisplayFrameNum = 2;
    ret = CVI_VDEC_SetChnParam(0, &p);
    printf("SetChnParam rc=%#x\n", ret);

    VDEC_CHN_POOL_S chnpool = { .hPicVbPool = pool, .hTmvVbPool = VB_INVALID_POOLID };
    {
        VDEC_MOD_PARAM_S modp;
        memset(&modp, 0, sizeof(modp));
        CVI_VDEC_GetModParam(&modp);
        modp.enVdecVBSource = VB_SOURCE_USER;
        int mr = CVI_VDEC_SetModParam(&modp);
        printf("SetModParam(USER) rc=%#x\n", mr);
    }
    ret = CVI_VDEC_AttachVbPool(0, &chnpool);
    printf("AttachVbPool rc=%#x\n", ret);

    ret = CVI_VDEC_StartRecvStream(0);
    printf("StartRecvStream rc=%#x\n", ret);
    if (ret != 0) return 2;

    static unsigned char buf[512 * 1024];
    FILE *f = fopen(jpg, "rb");
    size_t n = fread(buf, 1, sizeof(buf), f);
    fclose(f);
    VDEC_STREAM_S st;
    memset(&st, 0, sizeof(st));
    st.pu8Addr = buf;
    st.u32Len = n;
    st.bEndOfFrame = CVI_TRUE;
    st.bDisplay = 1;
    ret = CVI_VDEC_SendStream(0, &st, 3000);
    printf("SendStream(%zu B) rc=%#x\n", n, ret);

    VIDEO_FRAME_INFO_S fr;
    memset(&fr, 0, sizeof(fr));
    ret = CVI_VDEC_GetFrame(0, &fr, 3000);
    printf("GetFrame rc=%#x\n", ret);
    if (ret == 0) {
        printf("*** JPU 硬解成功: %ux%u stride=%u fmt=%d ***\n",
               fr.stVFrame.u32Width, fr.stVFrame.u32Height,
               fr.stVFrame.u32Stride[0], fr.stVFrame.enPixelFormat);
        CVI_VDEC_ReleaseFrame(0, &fr);
    }
    CVI_VDEC_StopRecvStream(0);
    CVI_VDEC_DetachVbPool(0);
    CVI_VDEC_ResetChn(0);
    CVI_VDEC_DestroyChn(0);
    return (ret == 0) ? 0 : 4;
}
