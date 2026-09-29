obj-m += vc_shim.o

KDIR ?= /home/sbh/ws/LicheeRV/LicheeRV-Nano-Build/linux_5.10/build/sg2002_licheervnano_sd
CROSS ?= /home/sbh/toolchains/riscv64-linux-musl-x86_64/bin/riscv64-unknown-linux-musl-

all:
	$(MAKE) ARCH=riscv -C $(KDIR) M=$(PWD) CROSS_COMPILE=$(CROSS) modules

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean
