// vc_shim.c — 为 sg200x-dev cvi_vc_driver 补齐旧 soph_vcodec 缺失的符号
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/wait.h>
#include <linux/sched.h>

static DEFINE_MUTEX(shim_vcodec_mutex);
static int shim_locked;

void vcodec_lock(void)   { mutex_lock(&shim_vcodec_mutex);   }
int  vcodec_trylock(void){ return mutex_trylock(&shim_vcodec_mutex) ? 1 : 0; }
void vcodec_unlock(void) { mutex_unlock(&shim_vcodec_mutex); }
int  vcodec_is_locked(void) { return shim_locked; }
long vpu_set_common_memory(unsigned long a, unsigned long b)
{ printk("vc_shim: vpu_set_common_memory(%lx,%lx) stub\n", a, b); return 0; }

EXPORT_SYMBOL(vcodec_lock);
EXPORT_SYMBOL(vcodec_trylock);
EXPORT_SYMBOL(vcodec_unlock);
EXPORT_SYMBOL(vcodec_is_locked);
EXPORT_SYMBOL(vpu_set_common_memory);

static int __init shim_init(void){ printk("vc_shim loaded\n"); return 0; }
static void __exit shim_exit(void){ printk("vc_shim unloaded\n"); }
module_init(shim_init);
module_exit(shim_exit);
MODULE_LICENSE("GPL");
