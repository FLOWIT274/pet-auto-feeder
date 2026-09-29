package com.licheerv.admin;

import android.app.Activity;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.SharedPreferences;
import android.net.Uri;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

/** 极简版：发现板子 → 展示 IP → 点击跳系统浏览器打开管理页 */
public class MainActivity extends Activity {
    private SharedPreferences prefs;
    private TextView status;
    private Button openBtn;

    private final BroadcastReceiver foundRc = new BroadcastReceiver() {
        @Override public void onReceive(Context c, Intent i) { refresh(); }
    };

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        prefs = getSharedPreferences("cfg", MODE_PRIVATE);

        if (!DiscService.running) {
            startForegroundService(new Intent(this, DiscService.class));
        }
        registerReceiver(foundRc, new IntentFilter("licheerv.FOUND"));

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setGravity(Gravity.CENTER);
        int pad = (int) (32 * getResources().getDisplayMetrics().density);
        root.setPadding(pad, pad, pad, pad);

        TextView title = new TextView(this);
        title.setText("LicheeRV 喂食管理台");
        title.setTextSize(22);
        title.setGravity(Gravity.CENTER);
        root.addView(title);

        status = new TextView(this);
        status.setTextSize(15);
        status.setGravity(Gravity.CENTER);
        status.setPadding(0, pad / 2, 0, pad);
        root.addView(status, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        openBtn = new Button(this);
        openBtn.setText("打开管理台");
        openBtn.setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) { openAdmin(); }
        });
        root.addView(openBtn, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        setContentView(root);
        refresh();
    }

    private void refresh() {
        String ip = prefs.getString("ip", null);
        int port = prefs.getInt("port", 8080);
        if (ip != null) {
            status.setText("已发现板子：\n\nhttp://" + ip + ":" + port
                    + "\n（" + prefs.getString("name", "") + "）");
            openBtn.setEnabled(true);
        } else {
            status.setText("正在搜索板子…\n\n请确认手机与板子连同一 WiFi");
            openBtn.setEnabled(false);
        }
    }

    private void openAdmin() {
        String ip = prefs.getString("ip", null);
        if (ip == null) return;
        startActivity(new Intent(Intent.ACTION_VIEW,
                Uri.parse("http://" + ip + ":" + prefs.getInt("port", 8080))));
    }

    @Override
    protected void onDestroy() {
        try { unregisterReceiver(foundRc); } catch (Exception ignored) { }
        super.onDestroy();
    }
}
