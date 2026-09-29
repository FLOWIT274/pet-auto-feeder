package com.licheerv.admin;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.net.Uri;
import android.net.wifi.WifiManager;
import android.os.IBinder;

import org.json.JSONObject;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.SocketTimeoutException;

public class DiscService extends Service {
    public static volatile boolean running = false;
    private static final int PORT = 37777;
    private static final String CH = "disc";
    private Thread worker;
    private WifiManager.MulticastLock mlock;

    @Override
    public IBinder onBind(Intent i) { return null; }

    @Override
    public int onStartCommand(Intent i, int f, int id) {
        if (running) return START_STICKY;
        running = true;
        try {
            WifiManager wm = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            mlock = wm.createMulticastLock("licheerv");
            mlock.setReferenceCounted(false);
            mlock.acquire();
        } catch (Exception ignored) { }
        startForegroundInternal();
        worker = new Thread(new Runnable() {
            @Override public void run() { loop(); }
        });
        worker.start();
        return START_STICKY;
    }

    private void startForegroundInternal() {
        NotificationManager nm = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (android.os.Build.VERSION.SDK_INT >= 26) {
            nm.createNotificationChannel(new NotificationChannel(CH, "板子发现",
                    NotificationManager.IMPORTANCE_DEFAULT));
        }
        Notification n = null;
        if (android.os.Build.VERSION.SDK_INT >= 26) {
            n = new Notification.Builder(this, CH)
                    .setContentTitle("喂食管理台发现服务")
                    .setContentText("正在搜索同一 WiFi 下的 LicheeRV 板子")
                    .setSmallIcon(android.R.drawable.stat_sys_download)
                    .setOngoing(true).build();
        } else {
            n = new Notification.Builder(this)
                    .setContentTitle("喂食管理台发现服务")
                    .setContentText("正在搜索同一 WiFi 下的 LicheeRV 板子")
                    .setSmallIcon(android.R.drawable.stat_sys_download)
                    .setOngoing(true).build();
        }
        startForeground(1, n);
    }

    private void loop() {
        SharedPreferences prefs = getSharedPreferences("cfg", MODE_PRIVATE);
        DatagramSocket sock = null;
        android.util.Log.i("licheerv", "发现线程启动, 监听 :" + PORT);
        try {
            sock = new DatagramSocket(PORT);
            sock.setBroadcast(true);
            sock.setSoTimeout(2000);
            byte[] probe = "LICHEERV-DISCOVER?".getBytes("UTF-8");
            long lastProbe = 0, lastNotify = 0;
            String lastIp = null;
            while (running) {
                long now = System.currentTimeMillis();
                // 探测：前 1 分钟每 4s，之后每 30s 保底
                long gap = (now - startedAt) < 60000 ? 4000 : 30000;
                if (now - lastProbe >= gap) {
                    try {
                        sock.send(new DatagramPacket(probe, probe.length,
                                InetAddress.getByName("255.255.255.255"), PORT));
                        android.util.Log.i("licheerv", "已发探测广播");
                    } catch (Exception e) {
                        android.util.Log.w("licheerv", "探测发送失败: " + e);
                    }
                    lastProbe = now;
                }
                byte[] buf = new byte[512];
                DatagramPacket p = new DatagramPacket(buf, buf.length);
                try {
                    sock.receive(p);
                    android.util.Log.i("licheerv", "收到包 " + p.getLength() + "B from "
                            + p.getAddress().getHostAddress());
                } catch (SocketTimeoutException e) {
                    continue;
                }
                // 解析失败（含自己的探测回环）只跳过本包，绝不终止监听线程
                JSONObject d;
                try {
                    d = new JSONObject(new String(p.getData(), 0, p.getLength(), "UTF-8"));
                } catch (Exception e) {
                    continue;
                }
                if (!"licheerv-admin".equals(d.optString("svc"))) continue;
                String ip = d.optString("ip"), name = d.optString("name", "");
                int port = d.optInt("port", 8080);
                if (ip.isEmpty()) continue;
                prefs.edit().putString("ip", ip).putInt("port", port)
                        .putString("name", name).apply();
                // 通知主界面自动加载/跟随新 IP
                Intent f = new Intent("licheerv.FOUND");
                f.setPackage(getPackageName());
                sendBroadcast(f);
                boolean isNew = !ip.equals(lastIp);
                lastIp = ip;
                if (isNew && now - lastNotify > 8000) {   // 同 IP 不重复打扰
                    notifyFound(ip, port, name);
                    lastNotify = now;
                }
            }
        } catch (Exception ignored) {
        } finally {
            if (sock != null) sock.close();
        }
    }

    private long startedAt = System.currentTimeMillis();

    private void notifyFound(String ip, int port, String name) {
        // 点击通知 → 系统浏览器直达管理页
        Intent open = new Intent(Intent.ACTION_VIEW, Uri.parse("http://" + ip + ":" + port));
        PendingIntent pi = PendingIntent.getActivity(this, 0, open,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        NotificationManager nm = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        Notification n;
        if (android.os.Build.VERSION.SDK_INT >= 26) {
            n = new Notification.Builder(this, CH)
                    .setContentTitle("发现 LicheeRV 管理台")
                    .setContentText(name + " → http://" + ip + ":" + port + "（点击打开）")
                    .setSmallIcon(android.R.drawable.stat_sys_download_done)
                    .setContentIntent(pi)
                    .setAutoCancel(true).build();
        } else {
            n = new Notification.Builder(this)
                    .setContentTitle("发现 LicheeRV 管理台")
                    .setContentText(name + " → http://" + ip + ":" + port + "（点击打开）")
                    .setSmallIcon(android.R.drawable.stat_sys_download_done)
                    .setContentIntent(pi)
                    .setAutoCancel(true).build();
        }
        nm.notify(2, n);
    }

    @Override
    public void onDestroy() {
        running = false;
        if (worker != null) worker.interrupt();
        if (mlock != null && mlock.isHeld()) mlock.release();
        super.onDestroy();
    }
}
