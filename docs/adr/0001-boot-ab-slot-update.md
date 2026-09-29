# Boot 分区 A/B 双槽更新（bootcount 自动回滚）

## Status: accepted

## Context

OTA 系统已能覆盖 rootfs（文件级 tar.gz，含断电保护），但 boot.sd（u-boot FIT：内核+设备树，含 ramdisk 时 11.2MB）无法安全更新：单槽直接覆盖存在"写坏起不来"和"新内核 panic"两个致命风险。boot 分区（FAT32 16MB）物理上放不下双镜像，u-boot 默认 env 不持久化（CONFIG_ENV_IS_NOWHERE），无法实现 bootcount 自动回滚。

## Decision

扩展 OTA 包支持 boot.sd 更新，采用 **A/B 双槽 + bootcount 自动回滚**（Android A/B 风格）：

1. **分区**：p1 BOOT 16MB → **24MB**，放 `boot_a.sd` + `boot_b.sd` + `fip.bin` + `uboot.env` + `uEnv.txt` + 标记文件（SD 重刷底包生效，genimage + partition_sd.xml 同步）。24MB 的由来：双槽含 ramdisk 旧镜像 22.4MB 已超 20MB 初定值；去 ramdisk 后双槽 18.4MB，24MB 留出余量（内核增长/重新加回 ramdisk 都放得下）。DATA 分区 16384KB → **16385KB**（CIMG 文件 = raw + 128B header，XmlParser 按实际文件大小校验，16384 恰好超 128 字节）
2. **去 ramdisk**：`CONFIG_SKIP_RAMDISK=y`（官方机制，build/Makefile `boot` target 对 multi.its 删 ramdisk 段）。boot.sd 11.2MB → **9.2MB**（8.78MiB 内核 + 21KB fdt）。ramdisk 对 SD 启动无用（/proc/cmdline 无 initrd=，root 直指 p2）
3. **u-boot 重编**（fip.bin）：defconfig 加 `CONFIG_ENV_IS_IN_FAT=y`（env 持久化到 FAT /boot/uboot.env，ENV_FAT_DEVICE_AND_PART="0:1"，文件默认 uboot.env）+ `CONFIG_BOOTCOUNT_LIMIT` + `CONFIG_BOOTCOUNT_ENV`。bootcount_env.c 门控：`upgrade_available=1` 才递增计数并 saveenv（减少 FAT 磨损）；`bootcount_load()` 在 upgrade_available=0 时返回 0 → 正常态永不触发回滚（残留计数安全）
4. **A/B 逻辑**：uEnv.txt（u-boot 启动 `env import -t` 自动导入）+ **mars-asic.h 编译进默认 env 的同一份**（altbootcmd 必须在默认 env 里——bootcount_error 判定发生在 bootdelay_process，早于 bootcmd 里 loadenvcmd 的导入）。bootcmd 按 `boot_slot` 变量选槽（bootab）；fatload 失败 fallback 另一槽（防文件损坏）；bootcount > bootlimit(3) 时 altbootcmd 自动切槽并清计数（防内核 panic）。cvipart.h 的 CONFIG_ENV_IS_NOWHERE 是 C 宏兜底（仅控制默认 env 编译，无 driver）——与 Kconfig 的 ENV_IS_IN_FAT 不冲突，env_save 走 fat driver
5. **Linux 侧**：**envtool.py**（自研，直接读写 /boot/uboot.env：CRC32 + 128KB data 区，原子写回）——buildroot 的 uboot-tools fw_setenv **不支持 FAT 后端**（fw_env.c 无 FAT 路径），故不能用于 FAT env；`S98bootok` init 脚本启动成功清 `upgrade_available` + `bootcount`
6. **升级流程**：OTA 包 `--boot <boot.sd>` 打进 `root/boot-update/boot.sd`（不用 root/boot/——/boot 是 FAT 挂载点，tar 覆盖会直接写进 FAT）→ ota-update.sh 校验通过后先调 `/usr/bin/boot-update.sh`（写非活动槽 tmp+rename+md5 回读 → env 切槽 boot_slot=<新槽> upgrade_available=1 bootcount=0）→ 再应用 rootfs。rootfs 应用中断时：新 boot + 旧 rootfs 启动失败 → bootcount 3 次后自动回滚旧槽（设计闭环）

## Considered Options

- **无 A/B（单槽+备份+手动恢复）**：不重编 u-boot，但内核 panic 无自动恢复，需物理介入。用户否决
- **轻量 A/B（仅 fallback 不自动回滚）**：uEnv.txt 可实现 fatload 失败 fallback，但内核 panic（FIT 合法但内核崩）无法自动回滚。用户否决
- **分区内 A/B 物理不可行**：16MB 装不下两个 boot.sd（含 ramdisk 时 22.4MB），须扩大分区
- **bootlimit=3**：1 次太敏感（偶发失败误回滚），5 次恢复太慢

## Consequences

- 底包必须重刷（分区表 24MB + 新 fip.bin），旧镜像无法直接 OTA 到新布局
- fip.bin 自身不参与 A/B（写坏只能 SD 重刷）——可接受，因为 fip.bin 基本不变
- u-boot env 持久化到 FAT 文件：uboot.env 损坏时回落默认 env（boot_slot 缺省走 A 槽），容错
- 每次升级需一次重启验证（新内核启动成功才完成切换）；回滚也需一次重启（第 4 次启动触发）
- 设备端新工具：envtool.py（env 读写）+ boot-update.sh（槽切换）+ S98bootok（清计数）+ ota-update.sh 增强（boot 应用段）
