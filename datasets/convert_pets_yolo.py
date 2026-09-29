#!/usr/bin/env python3
"""Oxford-IIIT Pets → YOLO 格式转换（37 类合并为 cat/dog 2 类）

输入：
  - train.parquet            (HF timm/oxford-iiit-pet, 含 image bytes + image_id + label)
  - annotations/xmls/*.xml   (官方 head bbox)
输出：
  - yolo/images/{train,val}/  + yolo/labels/{train,val}/
  - yolo/data.yaml
"""
import json
import os
import random
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq
from PIL import Image

BASE = Path(__file__).resolve().parent
ANNOT = BASE / "annotations" / "xmls"
OUT = BASE / "yolo"
VAL_RATIO = 0.1
SEED = 42

# Oxford Pets 37 类 → 物种（0=cat, 1=dog）
SPECIES = {
    "abyssinian": 0, "bengal": 0, "birman": 0, "bombay": 0,
    "british_shorthair": 0, "egyptian_mau": 0, "maine_coon": 0,
    "persian": 0, "ragdoll": 0, "russian_blue": 0, "siamese": 0, "sphynx": 0,
}
DOGS = [
    "american_bulldog", "american_pit_bull_terrier", "basset_hound", "beagle",
    "boxer", "chihuahua", "english_cocker_spaniel", "english_setter",
    "german_shorthaired", "great_pyrenees", "havanese", "japanese_chin",
    "keeshond", "leonberger", "miniature_pinscher", "newfoundland",
    "pomeranian", "pug", "saint_bernard", "samoyed", "scottish_terrier",
    "shiba_inu", "staffordshire_bull_terrier", "wheaten_terrier",
    "yorkshire_terrier",
]
for d in DOGS:
    SPECIES[d] = 1
assert len(SPECIES) == 37, f"类别映射应为 37 类，实际 {len(SPECIES)}"


def parse_xml(path):
    """返回 (species_id, xmin, ymin, xmax, ymax) 或 None（无 bbox）"""
    root = ET.parse(path).getroot()
    obj = root.find("object")
    if obj is None:
        return None
    name = obj.find("name").text.lower()
    if name not in ("cat", "dog"):
        # 兜底：用文件名前缀判断物种
        name = root.find("filename").text.split("_")[0].lower()
    sp = 0 if name == "cat" else 1
    b = obj.find("bndbox")
    xmin = float(b.find("xmin").text)
    ymin = float(b.find("ymin").text)
    xmax = float(b.find("xmax").text)
    ymax = float(b.find("ymax").text)
    return (sp, xmin, ymin, xmax, ymax)


def main():
    pq_path = BASE / "train.parquet"
    if not pq_path.exists():
        sys.exit(f"缺少 {pq_path}，请先下载")
    (OUT / "images" / "train").mkdir(parents=True, exist_ok=True)
    (OUT / "images" / "val").mkdir(parents=True, exist_ok=True)
    (OUT / "labels" / "train").mkdir(parents=True, exist_ok=True)
    (OUT / "labels" / "val").mkdir(parents=True, exist_ok=True)

    print("读取 parquet ...")
    table = pq.read_table(pq_path)
    df = table.to_pandas()
    print(f"  共 {len(df)} 行, 列: {list(df.columns)}")

    # 建立 xml 索引（image_id 不区分大小写）
    xml_map = {}
    for p in ANNOT.glob("*.xml"):
        xml_map[p.stem.lower()] = p

    ok, skip_no_xml, skip_bad = 0, 0, 0
    rows = []
    for i, row in df.iterrows():
        img_id = str(row["image_id"])
        xml_path = xml_map.get(img_id.lower())
        if xml_path is None:
            skip_no_xml += 1
            continue
        parsed = parse_xml(xml_path)
        if parsed is None:
            skip_bad += 1
            continue
        sp, xmin, ymin, xmax, ymax = parsed
        # 图片 bytes → 尺寸
        img_bytes = row["image"]["bytes"]
        img = Image.open(__import__("io").BytesIO(img_bytes))
        W, H = img.size
        # 归一化 + clamp
        cx = (xmin + xmax) / 2 / W
        cy = (ymin + ymax) / 2 / H
        w = (xmax - xmin) / W
        h = (ymax - ymin) / H
        cx, cy, w, h = max(0.0, min(1.0, cx)), max(0.0, min(1.0, cy)), \
                       max(0.001, min(1.0, w)), max(0.001, min(1.0, h))
        rows.append((img_id, sp, cx, cy, w, h, img_bytes))
        ok += 1
    print(f"  成功 {ok} | 无 xml {skip_no_xml} | 无 bbox {skip_bad}")

    # 划分 train/val（按 image_id 去重后随机）
    random.seed(SEED)
    unique = sorted({r[0] for r in rows})
    random.shuffle(unique)
    n_val = max(1, int(len(unique) * VAL_RATIO))
    val_ids = set(unique[:n_val])
    print(f"  train {len(unique) - n_val} | val {n_val}")

    stats = {0: 0, 1: 0}
    for img_id, sp, cx, cy, w, h, img_bytes in rows:
        split = "val" if img_id in val_ids else "train"
        img = Image.open(__import__("io").BytesIO(img_bytes)).convert("RGB")
        img.save(OUT / "images" / split / f"{img_id}.jpg", quality=95)
        with open(OUT / "labels" / split / f"{img_id}.txt", "w") as f:
            f.write(f"{sp} {cx:.6f} {cy:.6f} {w:.6f} {h:.6f}\n")
        stats[sp] += 1

    with open(OUT / "data.yaml", "w") as f:
        f.write("path: yolo\n")
        f.write("train: images/train\n")
        f.write("val: images/val\n")
        f.write("nc: 2\n")
        f.write("names: ['cat', 'dog']\n")

    print(f"\n类别分布: cat={stats[0]} dog={stats[1]}")
    print(f"完成，输出到 {OUT}")

    # 抽查 3 张可视化验证
    import io as _io
    vis_dir = BASE / "visual_check"
    vis_dir.mkdir(exist_ok=True)
    from PIL import ImageDraw
    sample = random.sample(rows, min(3, len(rows)))
    for img_id, sp, cx, cy, w, h, img_bytes in sample:
        img = Image.open(_io.BytesIO(img_bytes)).convert("RGB")
        d = ImageDraw.Draw(img)
        x1, y1 = (cx - w / 2) * img.width, (cy - h / 2) * img.height
        x2, y2 = (cx + w / 2) * img.width, (cy + h / 2) * img.height
        d.rectangle([x1, y1, x2, y2], outline="red" if sp == 0 else "blue", width=4)
        d.text((x1, max(0, y1 - 14)), "cat" if sp == 0 else "dog", fill="red" if sp == 0 else "blue")
        img.save(vis_dir / f"{img_id}.jpg")
    print(f"可视化抽查: {vis_dir}")


if __name__ == "__main__":
    main()
