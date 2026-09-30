# 基线 SDK 改动补丁

这些补丁针对 **Sipeed 基线 SDK**（<https://github.com/sipeed/LicheeRV-Nano-Build>），
用于复现本项目的硬件适配。基线本体不入库，只以补丁形式记录改动。

## 应用方式

```bash
git clone https://github.com/sipeed/LicheeRV-Nano-Build
cd LicheeRV-Nano-Build

# 方式一：一次全打（推荐，含全部 23 个文件）
git apply /path/to/pet-auto-feeder/patches/all.patch

# 方式二：按主题分别应用
git apply /path/to/pet-auto-feeder/patches/01-board-bringup.patch
git apply /path/to/pet-auto-feeder/patches/02-wifi-whitelist.patch
git apply /path/to/pet-auto-feeder/patches/03-dmesg-noise-silence.patch
```

补丁基线 commit：`af6ca9049`（"shell: show cwd in prompt"）。上游若已前进，可能需要 `git apply -3`。

## 三个补丁的内容

### 01-board-bringup.patch（16 个文件）
板级 bring-up：OV5647 MIPI 摄像头支持 + 蜂鸣器引脚安全化。

**蜂鸣器（A19/GPIOA 19）**：原厂把该脚配成 `UART1_RTS`。本项目蜂鸣器是**高电平触发**模块，
而 UART 复位态下 RTS 去断言会使该 pad 为**高** → 开机起持续长响。本项目不用 UART1
（BT 走 SDIO），故改为 `XGPIOA_19` 输出**低**（静音），从最早的可控点消除长响。

- `u-boot/cvi_board_init.c`：**关键安全改动**。`MIPIRX0N` 原本被配成 `CAM_MCLK1` 输出时钟，
  而本项目 bring-up 时该 pad 由外部 3.3V 供电（给 OV5647 供电）——输出时钟会直接短路电源轨、烧毁 SD 卡与主板。
  改为 `XGPIOC_10`（保持高阻输入）；`PWR_GPIO1` 也 pin 成普通 GPIO。
- `dts_riscv/...dts`：新增 `power-en-gpio`，保持高阻让外部 3.3V 驱动。
- `middleware/v2/component/isp/sensor/cv182x/ov_ov5647/*`：sensor 驱动适配（寄存器序列、参数表）。
- `middleware/v2/sample/vio/*`、`common/sample_common_sensor.c`、`sample.mk`：样例改为 OV5647，
  含 2-lane MIPI 的 lane 映射（CLK=PHY2, D0=PHY3, D1=PHY2）。
- `osdrv/interdrv/v2/{cif,vi,vpss}/*`：CIF/VI/VPSS 侧适配与帧导出调试函数。

### 02-wifi-whitelist.patch（2 个文件）
WiFi 白名单改造：只连 `/boot/wifi.ssid` 指定的网络，忽略 `/boot/wpa_supplicant.conf` 整体配置，
每次启动重建纯净配置（单 network + `update_config=0` + 无明文口令）。无凭证时进 AP 配网模式。

### 03-dmesg-noise-silence.patch（5 个文件）
清理内核日志噪声，便于用 dmesg 排查问题：AIC8800 WiFi/BT 驱动分级降噪、RTC 周期打印降为 debug。

## 校验

三个分片之和 = `all.patch`（均覆盖 23 个文件）：

```bash
grep -c '^diff --git' patches/0*.patch | awk -F: '{s+=$2} END{print s}'   # 23
grep -c '^diff --git' patches/all.patch                                    # 23
```

另有未纳入补丁的基线改动（`middleware/v2/install/` 构建产物等），属构建副产品，无需重现。
