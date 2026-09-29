package com.licheerv.admin;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.os.Build;

public class BootReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context ctx, Intent i) {
        if (Intent.ACTION_BOOT_COMPLETED.equals(i.getAction())) {
            Intent it = new Intent(ctx, DiscService.class);
            if (Build.VERSION.SDK_INT >= 26) {
                ctx.startForegroundService(it);
            } else {
                ctx.startService(it);
            }
        }
    }
}
