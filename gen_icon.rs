// 生成 memory-lens 图标：内存条 + 放大镜（透镜）造型，多尺寸 ICO。
// 运行: rustc -O gen_icon.rs -o gen_icon.exe && ./gen_icon.exe
use std::fs::File;
use std::io::Write;

// 每像素 RGBA 画到缓冲
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>, // RGBA
}
impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Canvas { w, h, px: vec![0; w * h * 4] }
    }
    fn set(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 { return; }
        let i = (y as usize * self.w + x as usize) * 4;
        // alpha 叠加（简单 src-over）
        let sa = a as u32;
        if sa >= 255 || self.px[i + 3] == 0 {
            self.px[i] = r; self.px[i + 1] = g; self.px[i + 2] = b; self.px[i + 3] = a;
        } else {
            let da = self.px[i + 3] as u32;
            let na = sa + da * (255 - sa) / 255;
            if na == 0 { return; }
            for c in 0..3 {
                let sc = [r, g, b][c] as u32;
                let dc = self.px[i + c] as u32;
                self.px[i + c] = ((sc * sa + dc * da * (255 - sa) / 255) / na) as u8;
            }
            self.px[i + 3] = na as u8;
        }
    }
    fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: (u8, u8, u8, u8)) {
        for y in y0..y1 { for x in x0..x1 { self.set(x, y, c.0, c.1, c.2, c.3); } }
    }
    // 圆角矩形
    fn fill_round(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, rad: i32, c: (u8, u8, u8, u8)) {
        for y in y0..y1 { for x in x0..x1 {
            let in_core = (x >= x0 + rad && x < x1 - rad) || (y >= y0 + rad && y < y1 - rad);
            if in_core { self.set(x, y, c.0, c.1, c.2, c.3); continue; }
            let cx = if x < x0 + rad { x0 + rad } else { x1 - rad - 1 };
            let cy = if y < y0 + rad { y0 + rad } else { y1 - rad - 1 };
            let dx = x - cx; let dy = y - cy;
            if dx * dx + dy * dy <= rad * rad { self.set(x, y, c.0, c.1, c.2, c.3); }
        } }
    }
    fn fill_circle(&mut self, cx: i32, cy: i32, rad: i32, c: (u8, u8, u8, u8)) {
        for y in (cy - rad)..=(cy + rad) { for x in (cx - rad)..=(cx + rad) {
            let dx = x - cx; let dy = y - cy;
            if dx * dx + dy * dy <= rad * rad { self.set(x, y, c.0, c.1, c.2, c.3); }
        } }
    }
    fn ring(&mut self, cx: i32, cy: i32, r_out: i32, r_in: i32, c: (u8, u8, u8, u8)) {
        for y in (cy - r_out)..=(cy + r_out) { for x in (cx - r_out)..=(cx + r_out) {
            let dx = x - cx; let dy = y - cy; let d2 = dx * dx + dy * dy;
            if d2 <= r_out * r_out && d2 >= r_in * r_in { self.set(x, y, c.0, c.1, c.2, c.3); }
        } }
    }
    // 旋转的粗线段（放大镜手柄）
    fn thick_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thick: i32, c: (u8, u8, u8, u8)) {
        let steps = ((x1 - x0).abs().max((y1 - y0).abs()) * 2.0) as i32 + 1;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = x0 + (x1 - x0) * t;
            let y = y0 + (y1 - y0) * t;
            self.fill_circle(x as i32, y as i32, thick / 2, c);
        }
    }
}

fn draw(size: usize) -> Canvas {
    let s = size as i32;
    let mut cv = Canvas::new(size, size);
    let u = s as f32 / 64.0; // 以 64 为基准缩放
    // 配色
    let blue = (76, 154, 255, 255u8);   // 主题蓝
    let dark = (26, 32, 44, 255u8);     // 深色内存条
    let gold = (253, 176, 34, 255u8);   // 金手指
    let light = (200, 230, 255, 255u8); // 镜片高光

    // 内存条主体（左上到右下倾斜放，简化：水平放置，留底部给放大镜）
    let mx0 = (6.0 * u) as i32; let my0 = (10.0 * u) as i32;
    let mx1 = (58.0 * u) as i32; let my1 = (34.0 * u) as i32;
    cv.fill_round(mx0, my0, mx1, my1, (4.0 * u) as i32, dark);
    // 内存条上的芯片（4 个蓝色小块）
    for i in 0..4 {
        let cw = (9.0 * u) as i32; let ch = (12.0 * u) as i32;
        let gap = (3.0 * u) as i32;
        let cx0 = mx0 + gap + i as i32 * (cw + gap);
        let cy0 = my0 + (5.0 * u) as i32;
        cv.fill_round(cx0, cy0, cx0 + cw, cy0 + ch, (2.0 * u) as i32, blue);
    }
    // 金手指（底部一排小金块）
    for i in 0..8 {
        let fw = (4.0 * u) as i32; let fh = (3.0 * u) as i32;
        let gap = (2.0 * u) as i32;
        let fx0 = mx0 + gap + i as i32 * (fw + gap);
        let fy0 = my1 - (4.0 * u) as i32;
        cv.fill_rect(fx0, fy0, fx0 + fw, fy0 + fh, gold);
    }
    // 放大镜（右下）：镜片圆环 + 手柄
    let ccx = (42.0 * u) as i32; let ccy = (40.0 * u) as i32;
    let r_out = (14.0 * u) as i32; let r_in = (10.0 * u) as i32;
    // 镜片玻璃（淡蓝半透明）
    cv.fill_circle(ccx, ccy, r_in, (140, 200, 255, 90));
    // 镜片里放大的芯片（一块更亮的蓝）
    cv.fill_round(ccx - (5.0*u) as i32, ccy - (4.0*u) as i32, ccx + (5.0*u) as i32, ccy + (4.0*u) as i32, (1.0*u) as i32, blue);
    // 圆环边框
    cv.ring(ccx, ccy, r_out, r_in, dark);
    cv.ring(ccx, ccy, r_in, r_in - (2.0*u).max(1.0) as i32, light);
    // 手柄（右下 45°）
    let hx0 = ccx as f32 + (r_out as f32) * 0.72;
    let hy0 = ccy as f32 + (r_out as f32) * 0.72;
    cv.thick_line(hx0, hy0, hx0 + 10.0 * u, hy0 + 10.0 * u, (6.0 * u) as i32, dark);
    cv
}

// 生成 BMP（BGRA 底向上）用于 ICO
fn bmp_data(cv: &Canvas) -> Vec<u8> {
    let w = cv.w; let h = cv.h;
    let mut out = Vec::with_capacity(w * h * 4);
    for y in (0..h).rev() {
        for x in 0..w {
            let i = (y * w + x) * 4;
            out.push(cv.px[i + 2]); // B
            out.push(cv.px[i + 1]); // G
            out.push(cv.px[i]);     // R
            out.push(cv.px[i + 3]); // A
        }
    }
    out
}
fn and_mask(w: usize, h: usize) -> Vec<u8> {
    // 1bpp AND mask，全 0（完全不透明由 alpha 决定）。每行补齐到 4 字节。
    let row_bytes = (w + 31) / 32 * 4;
    vec![0u8; row_bytes * h]
}

fn write_ico(path: &str, sizes: &[usize]) {
    let mut entries: Vec<(usize, Vec<u8>, Vec<u8>)> = Vec::new(); // (size, bmp, mask)
    for &sz in sizes {
        let cv = draw(sz);
        entries.push((sz, bmp_data(&cv), and_mask(sz, sz)));
    }
    let mut ico: Vec<u8> = Vec::new();
    // ICONDIR
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let header_size = 6 + 16 * sizes.len();
    let mut offset = header_size;
    // 先算每个图像的数据（BITMAPINFOHEADER + XOR + AND）
    let mut images: Vec<Vec<u8>> = Vec::new();
    for (sz, bmp, mask) in &entries {
        let header_size_b = 40u32;
        let xor_size = bmp.len() as u32;
        let and_size = mask.len() as u32;
        let mut img: Vec<u8> = Vec::new();
        img.extend_from_slice(&header_size_b.to_le_bytes());
        img.extend_from_slice(&(*sz as i32).to_le_bytes());      // width
        img.extend_from_slice(&(*sz as i32 * 2).to_le_bytes());  // height = 2*size (xor+and)
        img.extend_from_slice(&1u16.to_le_bytes());              // planes
        img.extend_from_slice(&32u16.to_le_bytes());             // bpp
        img.extend_from_slice(&0u32.to_le_bytes());              // compression
        img.extend_from_slice(&(xor_size + and_size).to_le_bytes());
        img.extend_from_slice(&0i32.to_le_bytes());              // x ppm
        img.extend_from_slice(&0i32.to_le_bytes());
        img.extend_from_slice(&0u32.to_le_bytes());
        img.extend_from_slice(&0u32.to_le_bytes());
        img.extend_from_slice(bmp);
        img.extend_from_slice(mask);
        images.push(img);
    }
    // ICONDIRENTRY
    for (i, (sz, _, _)) in entries.iter().enumerate() {
        let szb = if *sz >= 256 { 0u8 } else { *sz as u8 };
        ico.push(szb);            // width
        ico.push(szb);            // height
        ico.push(0);              // color count
        ico.push(0);              // reserved
        ico.extend_from_slice(&1u16.to_le_bytes());   // planes
        ico.extend_from_slice(&32u16.to_le_bytes());  // bpp
        ico.extend_from_slice(&(images[i].len() as u32).to_le_bytes()); // size
        ico.extend_from_slice(&(offset as u32).to_le_bytes());          // offset
        offset += images[i].len();
    }
    for img in &images { ico.extend_from_slice(img); }
    let mut f = File::create(path).unwrap();
    f.write_all(&ico).unwrap();
    println!("wrote {} ({} bytes, sizes {:?})", path, ico.len(), sizes);
}

fn main() {
    write_ico("icon.ico", &[16, 24, 32, 48, 64, 128, 256]);
}
