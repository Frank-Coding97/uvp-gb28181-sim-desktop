<script setup lang="ts">
// 室外红外变速球机（立杆吊装）3D 姿态模型。
//
// 运动链：
//   固定：立杆 + 抱箍 → 鹅颈吊臂 → 吊装接头 → 上壳 + 遮阳罩 + 方位刻度环
//   水平(pan)：回转环 → 云台头 → 左右叉臂
//   俯仰(tilt)：叉臂中间的机芯球 → 光学窗 → 镜头 + 两组红外灯
//   变倍(zoom)：镜头前伸 + 视场锥收窄 + 地面视场投影缩小
//
// 坐标约定：+Y 向上；pan=0 / tilt=0 时镜头朝 +Z。
// pan 正 = 向右（俯视顺时针），tilt 正 = 抬头，与 Ptz.vue 的命令语义一致。
// props 由页面按命令积分得出，组件内再用临界阻尼弹簧跟随，消除离散更新带来的顿挫。
import { computed, nextTick, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";
import * as THREE from "three";
import { RoomEnvironment } from "three/addons/environments/RoomEnvironment.js";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { RoundedBoxGeometry } from "three/addons/geometries/RoundedBoxGeometry.js";

const props = withDefaults(defineProps<{
  pan: number;
  tilt: number;
  zoom: number;
  active?: boolean;
  seeking?: boolean;
  online?: boolean;
  showFov?: boolean;
  zoomMax?: number;
  interactive?: boolean;
}>(), {
  active: false,
  seeking: false,
  online: true,
  showFov: true,
  zoomMax: 20,
  interactive: true,
});

const host = ref<HTMLDivElement | null>(null);
const webglError = ref("");

const WIDE_HFOV_DEG = 58;          // 1× 时水平视场角（典型 4.8mm 广角端）
const SENSOR_ASPECT = 16 / 9;
const FOV_LENGTH = 2.6;            // 视场锥可视长度
const FOOTPRINT_MAX_DIST = 14;     // 超过该距离不画地面投影（接近水平视线）
const MOUNT_Y = 1.46;              // 水平回转面高度
const BALL_OFFSET = -0.7;         // 俯仰轴相对回转面的偏移
const BALL_R = 0.42;
const WINDOW_ANGLE = 0.95;         // 光学窗开口半角（rad）
const WINDOW_BACK_Z = 0.15;        // 光学窗内底板深度
const DEFAULT_VIEW = new THREE.Vector3(3.2, 2.3, 4.2);
const VIEW_TARGET = new THREE.Vector3(0, 1.12, 0);

let renderer: THREE.WebGLRenderer | null = null;
let scene: THREE.Scene | null = null;
let viewCamera: THREE.PerspectiveCamera | null = null;
let controls: OrbitControls | null = null;
let envTarget: THREE.WebGLRenderTarget | null = null;
let resizeObserver: ResizeObserver | null = null;
let motionQuery: MediaQueryList | null = null;
let animationFrame = 0;
let lastFrameTime = 0;
let isActiveView = true;
let reducedMotion = false;

// 运动节点
let panGroup: THREE.Group | null = null;
let tiltGroup: THREE.Group | null = null;
let lensGroup: THREE.Group | null = null;
let fovGroup: THREE.Group | null = null;
let fovFill: THREE.MeshBasicMaterial | null = null;
let fovLine: THREE.LineBasicMaterial | null = null;
let footprint: THREE.Mesh | null = null;
let footprintOutline: THREE.LineLoop | null = null;
let aimDot: THREE.Mesh | null = null;
let statusLed: THREE.MeshStandardMaterial | null = null;
let pointerMat: THREE.MeshStandardMaterial | null = null;
let irMat: THREE.MeshStandardMaterial | null = null;

// 临界阻尼弹簧：value 跟随 target，velocity 自然衰减，无超调。
interface Spring { value: number; velocity: number; }
const panSpring: Spring = { value: 0, velocity: 0 };
const tiltSpring: Spring = { value: 0, velocity: 0 };
const zoomSpring: Spring = { value: 0, velocity: 0 }; // log(zoom)
const SPRING_OMEGA = 11;

function stepSpring(spring: Spring, target: number, dt: number): boolean {
  if (reducedMotion) {
    spring.value = target;
    spring.velocity = 0;
    return false;
  }
  const accel = SPRING_OMEGA * SPRING_OMEGA * (target - spring.value) - 2 * SPRING_OMEGA * spring.velocity;
  spring.velocity += accel * dt;
  spring.value += spring.velocity * dt;
  const settled = Math.abs(target - spring.value) < 0.01 && Math.abs(spring.velocity) < 0.01;
  if (settled) {
    spring.value = target;
    spring.velocity = 0;
  }
  return !settled;
}

/** 把目标 pan 展开到离当前值最近的等价角，跨 ±180° 时走短路径。 */
function unwrapPan(target: number, current: number): number {
  const delta = ((((target - current) % 360) + 540) % 360) - 180;
  return current + delta;
}

function clampTilt(value: number): number {
  return THREE.MathUtils.clamp(value, -90, 90);
}

function logZoom(value: number): number {
  return Math.log(THREE.MathUtils.clamp(value, 1, props.zoomMax));
}
function createMaterials() {
  return {
    shell: new THREE.MeshPhysicalMaterial({
      color: 0xe9edf1, metalness: 0.05, roughness: 0.38, clearcoat: 0.6, clearcoatRoughness: 0.28,
    }),
    shellShade: new THREE.MeshStandardMaterial({ color: 0xc9d0d7, metalness: 0.08, roughness: 0.46 }),
    graphite: new THREE.MeshStandardMaterial({ color: 0x2a3038, metalness: 0.35, roughness: 0.42 }),
    black: new THREE.MeshStandardMaterial({ color: 0x0b0e12, metalness: 0.2, roughness: 0.55 }),
    rubber: new THREE.MeshStandardMaterial({ color: 0x15181c, metalness: 0, roughness: 0.92 }),
    steel: new THREE.MeshStandardMaterial({ color: 0xb9c2cb, metalness: 0.9, roughness: 0.25 }),
    glass: new THREE.MeshPhysicalMaterial({
      color: 0x2b3a46, metalness: 0, roughness: 0.04, transparent: true, opacity: 0.32,
      clearcoat: 1, clearcoatRoughness: 0.02, side: THREE.DoubleSide, depthWrite: false,
    }),
    lensGlass: new THREE.MeshPhysicalMaterial({
      color: 0x06101c, metalness: 0.3, roughness: 0.02, clearcoat: 1, clearcoatRoughness: 0.01,
      iridescence: 0.6, iridescenceIOR: 1.6,
    }),
    ir: new THREE.MeshStandardMaterial({
      color: 0x2a0808, emissive: 0x6a0a0a, emissiveIntensity: 0.35, metalness: 0.2, roughness: 0.35,
    }),
    led: new THREE.MeshStandardMaterial({ color: 0x24c875, emissive: 0x0b8f52, emissiveIntensity: 1.4 }),
    pointer: new THREE.MeshStandardMaterial({ color: 0x3a8dff, emissive: 0x1a5fd8, emissiveIntensity: 0.9 }),
  };
}
type Materials = ReturnType<typeof createMaterials>;

function lathe(points: Array<[number, number]>, segments = 72): THREE.LatheGeometry {
  return new THREE.LatheGeometry(points.map(([x, y]) => new THREE.Vector2(x, y)), segments);
}

function add<T extends THREE.Object3D>(parent: THREE.Object3D, object: T, shadow = true): T {
  object.traverse((child) => {
    if (child instanceof THREE.Mesh) {
      child.castShadow = shadow;
      child.receiveShadow = shadow;
    }
  });
  parent.add(object);
  return object;
}

function mesh(
  parent: THREE.Object3D,
  geometry: THREE.BufferGeometry,
  material: THREE.Material,
  shadow = true,
): THREE.Mesh {
  return add(parent, new THREE.Mesh(geometry, material), shadow);
}

function canvasTexture(width: number, height: number, draw: (ctx: CanvasRenderingContext2D) => void): THREE.CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (ctx) draw(ctx);
  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  texture.anisotropy = 8;
  return texture;
}
// pan 角 θ 对应的水平朝向（pan 正 = 向右 = 俯视顺时针）。
function headingVector(deg: number, radius: number, y: number): THREE.Vector3 {
  const rad = THREE.MathUtils.degToRad(deg);
  return new THREE.Vector3(-Math.sin(rad) * radius, y, Math.cos(rad) * radius);
}

function buildFloor(root: THREE.Object3D): void {
  // 地面方位盘：0°/N 在 +Z（pan=0 的朝向），E 在 pan=+90°。
  const size = 1024;
  const texture = canvasTexture(size, size, (ctx) => {
    const c = size / 2;
    const scale = c / 4; // 1 世界单位 = 128px，盘面半径 4
    const point = (deg: number, r: number): [number, number] => {
      const rad = (deg * Math.PI) / 180;
      return [c - Math.sin(rad) * r * scale, c + Math.cos(rad) * r * scale];
    };
    ctx.strokeStyle = "rgba(140, 186, 236, 0.30)";
    ctx.lineWidth = 2;
    for (const r of [1, 2, 3]) {
      ctx.beginPath();
      ctx.arc(c, c, r * scale, 0, Math.PI * 2);
      ctx.stroke();
    }
    ctx.strokeStyle = "rgba(140, 186, 236, 0.16)";
    for (let deg = 0; deg < 360; deg += 30) {
      const [x0, y0] = point(deg, 0.55);
      const [x1, y1] = point(deg, 3.6);
      ctx.beginPath();
      ctx.moveTo(x0, y0);
      ctx.lineTo(x1, y1);
      ctx.stroke();
    }
    for (let deg = 0; deg < 360; deg += 5) {
      const major = deg % 30 === 0;
      const [x0, y0] = point(deg, major ? 3.42 : 3.52);
      const [x1, y1] = point(deg, 3.62);
      ctx.strokeStyle = major ? "rgba(170, 206, 245, 0.55)" : "rgba(140, 186, 236, 0.28)";
      ctx.lineWidth = major ? 3 : 2;
      ctx.beginPath();
      ctx.moveTo(x0, y0);
      ctx.lineTo(x1, y1);
      ctx.stroke();
    }
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.font = "700 34px 'Segoe UI', sans-serif";
    for (const [deg, text] of [[0, "N"], [90, "E"], [180, "S"], [270, "W"]] as const) {
      const [x, y] = point(deg, 3.84);
      ctx.fillStyle = deg === 0 ? "rgba(108, 172, 255, 0.95)" : "rgba(196, 220, 246, 0.7)";
      ctx.fillText(text, x, y);
    }
    ctx.font = "600 20px 'Consolas', monospace";
    ctx.fillStyle = "rgba(170, 200, 232, 0.5)";
    for (let deg = 30; deg < 360; deg += 30) {
      if (deg % 90 === 0) continue;
      const [x, y] = point(deg, 3.84);
      ctx.fillText(String(deg), x, y);
    }
  });
  const disc = new THREE.Mesh(
    new THREE.PlaneGeometry(8, 8),
    new THREE.MeshBasicMaterial({ map: texture, transparent: true, depthWrite: false }),
  );
  disc.rotation.x = -Math.PI / 2;
  disc.position.y = 0.001;
  disc.renderOrder = 0;
  root.add(disc);

  const shadowFloor = new THREE.Mesh(new THREE.PlaneGeometry(30, 30), new THREE.ShadowMaterial({ opacity: 0.35 }));
  shadowFloor.rotation.x = -Math.PI / 2;
  shadowFloor.receiveShadow = true;
  root.add(shadowFloor);
}

function buildFixed(root: THREE.Object3D, m: Materials): void {
  const fixed = new THREE.Group();
  fixed.name = "fixed-mount";
  root.add(fixed);

  // 立杆 + 抱箍：杆从地面立起，吊臂根部用两道不锈钢抱箍固定。
  const POLE_Z = -1.06;
  const pole = mesh(fixed, new THREE.CylinderGeometry(0.075, 0.09, MOUNT_Y + 0.95, 32), m.shellShade);
  pole.position.set(0, (MOUNT_Y + 0.95) / 2, POLE_Z);
  const poleCap = mesh(fixed, new THREE.SphereGeometry(0.076, 24, 12, 0, Math.PI * 2, 0, Math.PI / 2), m.shellShade);
  poleCap.position.set(0, MOUNT_Y + 0.95, POLE_Z);
  const poleFoot = mesh(fixed, new THREE.CylinderGeometry(0.2, 0.22, 0.04, 32), m.graphite);
  poleFoot.position.set(0, 0.02, POLE_Z);
  const bracket = mesh(fixed, new RoundedBoxGeometry(0.2, 0.34, 0.06, 3, 0.02), m.shell);
  bracket.position.set(0, MOUNT_Y + 0.66, POLE_Z + 0.1);
  for (const dy of [-0.11, 0.11]) {
    const hoop = mesh(fixed, new THREE.TorusGeometry(0.088, 0.012, 8, 40), m.steel);
    hoop.rotation.x = Math.PI / 2;
    hoop.position.set(0, MOUNT_Y + 0.66 + dy, POLE_Z);
  }
  const boss = mesh(fixed, new THREE.CylinderGeometry(0.1, 0.12, 0.06, 32), m.shell);
  boss.rotation.x = Math.PI / 2;
  boss.position.set(0, MOUNT_Y + 0.66, POLE_Z + 0.15);

  // 鹅颈吊臂：先平伸再下弯到球机正上方。
  const curve = new THREE.CatmullRomCurve3([
    new THREE.Vector3(0, MOUNT_Y + 0.66, -0.9),
    new THREE.Vector3(0, MOUNT_Y + 0.7, -0.6),
    new THREE.Vector3(0, MOUNT_Y + 0.66, -0.2),
    new THREE.Vector3(0, MOUNT_Y + 0.56, -0.02),
    new THREE.Vector3(0, MOUNT_Y + 0.5, 0),
  ], false, "centripetal");
  mesh(fixed, new THREE.TubeGeometry(curve, 64, 0.075, 24, false), m.shell);

  // 吊装接头：螺纹套 + 锁紧环。
  const coupler = mesh(fixed, new THREE.CylinderGeometry(0.1, 0.12, 0.14, 32), m.shellShade);
  coupler.position.set(0, MOUNT_Y + 0.46, 0);
  const lock = mesh(fixed, new THREE.TorusGeometry(0.11, 0.014, 10, 40), m.steel);
  lock.rotation.x = Math.PI / 2;
  lock.position.set(0, MOUNT_Y + 0.42, 0);

  // 上壳（固定）：内含水平电机和滑环。
  mesh(fixed, lathe([
    [0, MOUNT_Y + 0.4],
    [0.12, MOUNT_Y + 0.4],
    [0.3, MOUNT_Y + 0.36],
    [0.44, MOUNT_Y + 0.27],
    [0.5, MOUNT_Y + 0.16],
    [0.51, MOUNT_Y + 0.06],
    [0.5, MOUNT_Y + 0.02],
    [0, MOUNT_Y + 0.02],
  ]), m.shell);

  // 遮阳罩：顶部外扩一圈，带下翻边。
  mesh(fixed, lathe([
    [0.13, MOUNT_Y + 0.43],
    [0.36, MOUNT_Y + 0.39],
    [0.52, MOUNT_Y + 0.3],
    [0.58, MOUNT_Y + 0.22],
    [0.585, MOUNT_Y + 0.19],
    [0.57, MOUNT_Y + 0.19],
    [0.56, MOUNT_Y + 0.22],
    [0.5, MOUNT_Y + 0.29],
    [0.35, MOUNT_Y + 0.37],
    [0.13, MOUNT_Y + 0.405],
  ]), m.shell);

  // 方位刻度环：固定在上壳下沿，配合回转环上的蓝色指针读 pan。
  const scaleRing = mesh(fixed, new THREE.CylinderGeometry(0.505, 0.505, 0.05, 72, 1, true), m.graphite);
  scaleRing.position.y = MOUNT_Y + 0.025;
  const tickGeo = new THREE.BoxGeometry(0.008, 0.024, 0.006);
  const tickMajorGeo = new THREE.BoxGeometry(0.012, 0.04, 0.006);
  for (let deg = 0; deg < 360; deg += 10) {
    const major = deg % 90 === 0;
    const tick = mesh(fixed, major ? tickMajorGeo : tickGeo, major ? m.pointer : m.steel, false);
    tick.position.copy(headingVector(deg, 0.508, MOUNT_Y + (major ? 0.028 : 0.032)));
    tick.rotation.y = -THREE.MathUtils.degToRad(deg);
  }
}
function buildPanHead(root: THREE.Object3D, m: Materials): void {
  panGroup = new THREE.Group();
  panGroup.name = "pan";
  panGroup.position.y = MOUNT_Y;
  root.add(panGroup);

  // 回转环 + 方位指针（指针永远在镜头正上方，对着刻度环读数）。
  const ring = mesh(panGroup, new THREE.CylinderGeometry(0.49, 0.49, 0.06, 72), m.graphite);
  ring.position.y = -0.03;
  const gap = mesh(panGroup, new THREE.TorusGeometry(0.492, 0.006, 8, 72), m.black, false);
  gap.rotation.x = Math.PI / 2;
  gap.position.y = -0.002;
  const pointer = mesh(panGroup, new THREE.BoxGeometry(0.03, 0.05, 0.018), m.pointer, false);
  pointer.position.set(0, -0.03, 0.495);
  pointerMat = m.pointer;

  // 云台头：上宽下窄的扁壳，底面接两侧叉臂。
  mesh(panGroup, lathe([
    [0, -0.06],
    [0.48, -0.06],
    [0.5, -0.09],
    [0.5, -0.17],
    [0.47, -0.22],
    [0.38, -0.25],
    [0, -0.25],
  ]), m.shell);
  const seam = mesh(panGroup, new THREE.TorusGeometry(0.5, 0.006, 8, 72), m.shellShade, false);
  seam.rotation.x = Math.PI / 2;
  seam.position.y = -0.13;

  // 状态灯：云台头正面。
  const led = mesh(panGroup, new THREE.SphereGeometry(0.018, 16, 12), m.led, false);
  led.position.set(0.12, -0.15, 0.496);
  statusLed = m.led;

  // 左右叉臂 + 俯仰轴端盖。
  for (const side of [-1, 1]) {
    const arm = mesh(panGroup, new RoundedBoxGeometry(0.11, 0.64, 0.4, 4, 0.045), m.shell);
    arm.position.set(side * 0.505, -0.55, -0.02);
    const cap = mesh(panGroup, new THREE.CylinderGeometry(0.13, 0.13, 0.03, 40), m.shellShade);
    cap.rotation.z = Math.PI / 2;
    cap.position.set(side * 0.572, BALL_OFFSET, 0);
    const bolt = mesh(panGroup, new THREE.CylinderGeometry(0.035, 0.035, 0.012, 6), m.steel, false);
    bolt.rotation.z = Math.PI / 2;
    bolt.position.set(side * 0.59, BALL_OFFSET, 0);
    const axle = mesh(panGroup, new THREE.CylinderGeometry(0.05, 0.05, 0.06, 24), m.graphite);
    axle.rotation.z = Math.PI / 2;
    axle.position.set(side * 0.44, BALL_OFFSET, 0);
  }

  // 品牌标识：印在两侧叉臂外侧平面（端盖上方的空白区），随水平转动。
  // 平面绕 Y 转 ±90° 贴到 ±X 面；从外侧看文字都是正向从左到右。
  const logoMat = new THREE.MeshStandardMaterial({
    map: createLogoTexture(),
    transparent: true,
    roughness: 0.42,
    metalness: 0.05,
    depthWrite: false,
    polygonOffset: true,
    polygonOffsetFactor: -2,
  });
  const logoGeo = new THREE.PlaneGeometry(0.3, 0.1125);
  for (const side of [-1, 1]) {
    const logo = mesh(panGroup, logoGeo, logoMat, false);
    logo.rotation.y = side * Math.PI / 2;
    logo.position.set(side * 0.5615, -0.4, -0.02);
  }
}

function createLogoTexture(): THREE.CanvasTexture {
  return canvasTexture(512, 192, (ctx) => {
    ctx.clearRect(0, 0, 512, 192);
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillStyle = "#1f3f73";
    ctx.font = "800 132px 'Segoe UI', 'Arial Black', sans-serif";
    ctx.fillText("UVP", 256, 88);
    // 字下方一道品牌蓝细线，和方位指针同色。
    ctx.fillStyle = "#3a8dff";
    ctx.fillRect(176, 160, 160, 10);
  });
}

function buildTiltBall(m: Materials): void {
  if (!panGroup) return;
  tiltGroup = new THREE.Group();
  tiltGroup.name = "tilt";
  tiltGroup.position.y = BALL_OFFSET;
  panGroup.add(tiltGroup);

  // 机芯球壳：正面挖出圆形光学窗（球极轴转到 +Z）。
  const shellGeo = new THREE.SphereGeometry(BALL_R, 72, 48, 0, Math.PI * 2, WINDOW_ANGLE, Math.PI - WINDOW_ANGLE);
  shellGeo.rotateX(Math.PI / 2);
  // 开口处能看到球壳内壁，单独一份双面材质，避免影响其他外壳件。
  const shellInside = m.shell.clone();
  shellInside.side = THREE.DoubleSide;
  mesh(tiltGroup, shellGeo, shellInside);

  // 球壳分型线：竖直一圈，随俯仰转动，直观看出 tilt。
  const seamGeo = new THREE.TorusGeometry(BALL_R + 0.002, 0.005, 8, 96, Math.PI * 2 - WINDOW_ANGLE * 2);
  const seam = mesh(tiltGroup, seamGeo, m.shellShade, false);
  // 局部 Z 先转 π+W 让缺口对准光学窗，再绕 Y 立起到 YZ 平面。
  seam.rotation.set(0, Math.PI / 2, Math.PI + WINDOW_ANGLE);

  // 光学窗：黑色内腔 + 底板 + 弧面玻璃。
  const rimR = BALL_R * Math.sin(WINDOW_ANGLE);
  const rimZ = BALL_R * Math.cos(WINDOW_ANGLE);
  const wellMat = m.black.clone();
  wellMat.side = THREE.DoubleSide;
  const well = mesh(tiltGroup, new THREE.CylinderGeometry(rimR, rimR * 0.98, rimZ - WINDOW_BACK_Z, 64, 1, true), wellMat);
  well.rotation.x = Math.PI / 2;
  well.position.z = (rimZ + WINDOW_BACK_Z) / 2;
  const back = mesh(tiltGroup, new THREE.CircleGeometry(rimR, 64), m.black);
  back.position.z = WINDOW_BACK_Z;
  const bezel = mesh(tiltGroup, new THREE.TorusGeometry(rimR, 0.012, 10, 72), m.graphite, false);
  bezel.position.z = rimZ;

  const glassGeo = new THREE.SphereGeometry(BALL_R + 0.001, 64, 16, 0, Math.PI * 2, 0, WINDOW_ANGLE);
  glassGeo.rotateX(Math.PI / 2);
  const glass = mesh(tiltGroup, glassGeo, m.glass, false);
  glass.renderOrder = 5;

  // 红外灯板：镜头两侧各 2×3 颗。
  irMat = m.ir;
  const irGeo = new THREE.SphereGeometry(0.02, 16, 10);
  const boardGeo = new RoundedBoxGeometry(0.1, 0.2, 0.012, 2, 0.006);
  for (const side of [-1, 1]) {
    const board = mesh(tiltGroup, boardGeo, m.graphite, false);
    board.position.set(side * 0.215, 0, WINDOW_BACK_Z + 0.008);
    for (let col = 0; col < 2; col += 1) {
      for (let row = 0; row < 3; row += 1) {
        const ir = mesh(tiltGroup, irGeo, m.ir, false);
        ir.position.set(side * (0.19 + col * 0.05), (row - 1) * 0.062, WINDOW_BACK_Z + 0.02);
      }
    }
  }
  // 光敏/激光测距小窗。
  const sensor = mesh(tiltGroup, new THREE.CircleGeometry(0.016, 20), m.lensGlass, false);
  sensor.position.set(0, 0.2, WINDOW_BACK_Z + 0.002);

  // 镜头：固定外套筒 + 可伸缩内筒（变倍时前伸）。
  const sleeve = mesh(tiltGroup, new THREE.CylinderGeometry(0.135, 0.14, 0.06, 48), m.graphite);
  sleeve.rotation.x = Math.PI / 2;
  sleeve.position.z = WINDOW_BACK_Z + 0.03;

  lensGroup = new THREE.Group();
  tiltGroup.add(lensGroup);
  const tube = mesh(lensGroup, new THREE.CylinderGeometry(0.112, 0.118, 0.12, 48), m.black);
  tube.rotation.x = Math.PI / 2;
  tube.position.z = -0.06;
  const grip = mesh(lensGroup, new THREE.CylinderGeometry(0.119, 0.119, 0.03, 48, 1, true), m.rubber, false);
  grip.rotation.x = Math.PI / 2;
  grip.position.z = -0.08;
  const frontRing = mesh(lensGroup, new THREE.TorusGeometry(0.106, 0.01, 10, 48), m.steel, false);
  frontRing.position.z = 0;
  const front = mesh(lensGroup, new THREE.CircleGeometry(0.1, 48), m.lensGlass, false);
  front.position.z = -0.004;
  const pupil = mesh(lensGroup, new THREE.CircleGeometry(0.05, 40), new THREE.MeshBasicMaterial({ color: 0x010306 }), false);
  pupil.position.z = -0.003;
  const glint = mesh(lensGroup, new THREE.CircleGeometry(0.014, 16), new THREE.MeshBasicMaterial({ color: 0xcfeaff }), false);
  glint.position.set(-0.035, 0.035, -0.002);
}
function halfFov(zoom: number): { h: number; v: number } {
  const wideH = THREE.MathUtils.degToRad(WIDE_HFOV_DEG / 2);
  const h = Math.atan(Math.tan(wideH) / Math.max(1, zoom));
  const v = Math.atan(Math.tan(h) / SENSOR_ASPECT);
  return { h, v };
}

function buildFov(): void {
  if (!lensGroup) return;
  fovGroup = new THREE.Group();
  fovGroup.name = "fov";
  lensGroup.add(fovGroup);

  // 单位视锥：顶点在原点，底面在 z=1 的 [-1,1]² 矩形；每帧按视场角缩放。
  const apex = [0, 0, 0];
  const corners = [[-1, -1, 1], [1, -1, 1], [1, 1, 1], [-1, 1, 1]];
  const positions: number[] = [];
  for (let i = 0; i < 4; i += 1) {
    const a = corners[i];
    const b = corners[(i + 1) % 4];
    positions.push(...apex, ...a, ...b);
  }
  const coneGeo = new THREE.BufferGeometry();
  coneGeo.setAttribute("position", new THREE.Float32BufferAttribute(positions, 3));
  fovFill = new THREE.MeshBasicMaterial({
    color: 0x4aa3ff, transparent: true, opacity: 0.07, side: THREE.DoubleSide,
    depthWrite: false, blending: THREE.AdditiveBlending,
  });
  const cone = new THREE.Mesh(coneGeo, fovFill);
  cone.renderOrder = 3;
  fovGroup.add(cone);

  const edges: number[] = [];
  for (let i = 0; i < 4; i += 1) {
    edges.push(...apex, ...corners[i]);
    edges.push(...corners[i], ...corners[(i + 1) % 4]);
  }
  const edgeGeo = new THREE.BufferGeometry();
  edgeGeo.setAttribute("position", new THREE.Float32BufferAttribute(edges, 3));
  fovLine = new THREE.LineBasicMaterial({ color: 0x6cb6ff, transparent: true, opacity: 0.5, depthWrite: false });
  const lines = new THREE.LineSegments(edgeGeo, fovLine);
  lines.renderOrder = 3;
  fovGroup.add(lines);
}

function buildFootprint(root: THREE.Object3D): void {
  // 地面视场投影：四条角射线与地面求交，每帧更新顶点。
  const geo = new THREE.BufferGeometry();
  geo.setAttribute("position", new THREE.Float32BufferAttribute(new Float32Array(4 * 3), 3));
  geo.setIndex([0, 1, 2, 0, 2, 3]);
  footprint = new THREE.Mesh(geo, new THREE.MeshBasicMaterial({
    color: 0x3a8dff, transparent: true, opacity: 0.18, depthWrite: false, side: THREE.DoubleSide,
  }));
  footprint.renderOrder = 1;
  footprint.frustumCulled = false;
  root.add(footprint);

  const outlineGeo = new THREE.BufferGeometry();
  outlineGeo.setAttribute("position", new THREE.Float32BufferAttribute(new Float32Array(4 * 3), 3));
  footprintOutline = new THREE.LineLoop(outlineGeo, new THREE.LineBasicMaterial({
    color: 0x7cc0ff, transparent: true, opacity: 0.85, depthWrite: false,
  }));
  footprintOutline.renderOrder = 1;
  footprintOutline.frustumCulled = false;
  root.add(footprintOutline);

  aimDot = new THREE.Mesh(new THREE.RingGeometry(0.035, 0.06, 32), new THREE.MeshBasicMaterial({
    color: 0xbfe0ff, transparent: true, opacity: 0.9, depthWrite: false,
  }));
  aimDot.rotation.x = -Math.PI / 2;
  aimDot.renderOrder = 2;
  root.add(aimDot);
}

const tmpOrigin = new THREE.Vector3();
const tmpDir = new THREE.Vector3();
const tmpQuat = new THREE.Quaternion();

/** 镜头局部方向 (x,y,1) 打到地面 y=0 的交点；朝上或太远返回 null。 */
function groundHit(localX: number, localY: number, out: THREE.Vector3): boolean {
  if (!lensGroup) return false;
  lensGroup.getWorldPosition(tmpOrigin);
  lensGroup.getWorldQuaternion(tmpQuat);
  tmpDir.set(localX, localY, 1).normalize().applyQuaternion(tmpQuat);
  if (tmpDir.y >= -1e-3) return false;
  const t = -tmpOrigin.y / tmpDir.y;
  if (t > FOOTPRINT_MAX_DIST) return false;
  out.copy(tmpOrigin).addScaledVector(tmpDir, t);
  out.y = 0.004;
  return true;
}

const hitPoint = new THREE.Vector3();
function updateFov(zoom: number): void {
  const { h, v } = halfFov(zoom);
  const visible = props.showFov && props.online;
  const hot = props.active || props.seeking;
  if (fovGroup) {
    fovGroup.visible = visible;
    fovGroup.scale.set(Math.tan(h) * FOV_LENGTH, Math.tan(v) * FOV_LENGTH, FOV_LENGTH);
  }
  if (fovFill) fovFill.opacity = hot ? 0.11 : 0.07;
  if (fovLine) fovLine.opacity = hot ? 0.7 : 0.45;

  if (!footprint || !footprintOutline || !aimDot) return;
  const tx = Math.tan(h);
  const ty = Math.tan(v);
  const corners: Array<[number, number]> = [[-tx, -ty], [tx, -ty], [tx, ty], [-tx, ty]];
  const fill = footprint.geometry.getAttribute("position") as THREE.BufferAttribute;
  const outline = footprintOutline.geometry.getAttribute("position") as THREE.BufferAttribute;
  let all = visible;
  for (let i = 0; i < 4 && all; i += 1) {
    if (!groundHit(corners[i][0], corners[i][1], hitPoint)) all = false;
    else {
      fill.setXYZ(i, hitPoint.x, hitPoint.y, hitPoint.z);
      outline.setXYZ(i, hitPoint.x, hitPoint.y, hitPoint.z);
    }
  }
  footprint.visible = all;
  footprintOutline.visible = all;
  fill.needsUpdate = true;
  outline.needsUpdate = true;
  footprint.geometry.computeBoundingSphere();

  const aim = visible && groundHit(0, 0, hitPoint);
  aimDot.visible = aim;
  if (aim) aimDot.position.set(hitPoint.x, 0.005, hitPoint.z);
}
function buildScene(): void {
  const element = host.value;
  if (!element) return;
  try {
    renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true, powerPreference: "high-performance" });
  } catch (error) {
    webglError.value = `WebGL 初始化失败：${String(error)}`;
    return;
  }
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.05;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  renderer.domElement.setAttribute("aria-label", "云台球机三维实时姿态");
  renderer.domElement.addEventListener("webglcontextlost", onContextLost);
  renderer.domElement.addEventListener("webglcontextrestored", onContextRestored);
  element.appendChild(renderer.domElement);

  scene = new THREE.Scene();
  viewCamera = new THREE.PerspectiveCamera(32, 1, 0.1, 100);
  viewCamera.position.copy(DEFAULT_VIEW);

  const pmrem = new THREE.PMREMGenerator(renderer);
  const room = new RoomEnvironment();
  envTarget = pmrem.fromScene(room, 0.04);
  room.traverse((o) => {
    if (o instanceof THREE.Mesh) {
      o.geometry.dispose();
      (o.material as THREE.Material).dispose();
    }
  });
  pmrem.dispose();
  scene.environment = envTarget.texture;
  scene.environmentIntensity = 0.7;

  scene.add(new THREE.HemisphereLight(0xeaf4ff, 0x0b1420, 0.5));
  const key = new THREE.DirectionalLight(0xfff4e6, 2.4);
  key.position.set(4, 7, 5);
  key.castShadow = true;
  key.shadow.mapSize.set(2048, 2048);
  key.shadow.camera.left = -3;
  key.shadow.camera.right = 3;
  key.shadow.camera.top = 4;
  key.shadow.camera.bottom = -3;
  key.shadow.camera.near = 1;
  key.shadow.camera.far = 20;
  key.shadow.bias = -0.0003;
  key.shadow.normalBias = 0.02;
  scene.add(key);
  const rim = new THREE.DirectionalLight(0x7fb2ff, 1.6);
  rim.position.set(-5, 3, -4);
  scene.add(rim);

  const materials = createMaterials();
  buildFloor(scene);
  buildFixed(scene, materials);
  buildPanHead(scene, materials);
  buildTiltBall(materials);
  buildFov();
  buildFootprint(scene);

  controls = new OrbitControls(viewCamera, renderer.domElement);
  controls.target.copy(VIEW_TARGET);
  controls.enableDamping = true;
  controls.dampingFactor = 0.09;
  controls.enablePan = false;
  controls.minDistance = 2.6;
  controls.maxDistance = 11;
  controls.maxPolarAngle = THREE.MathUtils.degToRad(88);
  controls.enabled = props.interactive;
  controls.addEventListener("change", requestRender);
  controls.update();

  panSpring.value = props.pan;
  tiltSpring.value = clampTilt(props.tilt);
  zoomSpring.value = logZoom(props.zoom);
  applyPose();

  resizeObserver = new ResizeObserver(resize);
  resizeObserver.observe(element);
  resize();
}

function applyPose(): void {
  if (!panGroup || !tiltGroup || !lensGroup) return;
  panGroup.rotation.y = -THREE.MathUtils.degToRad(panSpring.value);
  tiltGroup.rotation.x = -THREE.MathUtils.degToRad(tiltSpring.value);
  const zoom = Math.exp(zoomSpring.value);
  const progress = zoomSpring.value / Math.log(props.zoomMax);
  lensGroup.position.z = WINDOW_BACK_Z + 0.07 + THREE.MathUtils.clamp(progress, 0, 1) * 0.065;
  panGroup.updateMatrixWorld(true);
  updateFov(zoom);
}

function updateIndicators(time: number): boolean {
  const hot = props.active || props.seeking;
  if (statusLed) {
    const pulse = reducedMotion ? 1.6 : 1.4 + Math.sin(time * 0.008) * 0.8;
    if (!props.online) {
      statusLed.color.setHex(0x59636e);
      statusLed.emissive.setHex(0x000000);
    } else if (hot) {
      statusLed.color.setHex(0x3a8dff);
      statusLed.emissive.setHex(0x1a63ff);
      statusLed.emissiveIntensity = pulse;
    } else {
      statusLed.color.setHex(0x24c875);
      statusLed.emissive.setHex(0x0b8f52);
      statusLed.emissiveIntensity = 1.4;
    }
  }
  if (pointerMat) pointerMat.emissiveIntensity = hot ? 1.6 : 0.9;
  if (irMat) irMat.emissiveIntensity = props.online ? 0.45 : 0.05;
  return hot && props.online && !reducedMotion;
}

function frame(time: number): void {
  animationFrame = 0;
  if (!renderer || !scene || !viewCamera) return;
  const dt = lastFrameTime === 0 ? 1 / 60 : Math.min(0.05, (time - lastFrameTime) / 1000);
  lastFrameTime = time;

  const panTarget = unwrapPan(props.pan, panSpring.value);
  let moving = stepSpring(panSpring, panTarget, dt);
  moving = stepSpring(tiltSpring, clampTilt(props.tilt), dt) || moving;
  moving = stepSpring(zoomSpring, logZoom(props.zoom), dt) || moving;
  applyPose();
  const pulsing = updateIndicators(time);
  const orbiting = controls?.update() ?? false;

  renderer.render(scene, viewCamera);
  if ((moving || pulsing || orbiting) && isActiveView) {
    animationFrame = requestAnimationFrame(frame);
  } else {
    lastFrameTime = 0;
  }
}

function requestRender(): void {
  if (!isActiveView || animationFrame !== 0) return;
  animationFrame = requestAnimationFrame(frame);
}

function resize(): void {
  if (!host.value || !renderer || !viewCamera) return;
  const width = Math.max(1, host.value.clientWidth);
  const height = Math.max(1, host.value.clientHeight);
  renderer.setSize(width, height, false);
  viewCamera.aspect = width / height;
  // 窄画面拉远一点，保证整机 + 地面投影都在画内。
  viewCamera.fov = viewCamera.aspect < 1.1 ? 38 : 32;
  viewCamera.updateProjectionMatrix();
  requestRender();
}

function resetView(): void {
  if (!viewCamera || !controls) return;
  viewCamera.position.copy(DEFAULT_VIEW);
  controls.target.copy(VIEW_TARGET);
  controls.update();
  requestRender();
}
defineExpose({ resetView });

function onMotionPreference(event: MediaQueryListEvent | MediaQueryList): void {
  reducedMotion = event.matches;
  requestRender();
}

function onContextLost(event: Event): void {
  event.preventDefault();
  if (animationFrame !== 0) cancelAnimationFrame(animationFrame);
  animationFrame = 0;
  webglError.value = "WebGL 上下文已丢失，正在等待图形系统恢复…";
}

function onContextRestored(): void {
  webglError.value = "";
  resize();
}

function disposeScene(): void {
  if (animationFrame !== 0) cancelAnimationFrame(animationFrame);
  animationFrame = 0;
  resizeObserver?.disconnect();
  resizeObserver = null;
  motionQuery?.removeEventListener("change", onMotionPreference);
  motionQuery = null;
  controls?.removeEventListener("change", requestRender);
  controls?.dispose();
  controls = null;
  if (scene) {
    const geometries = new Set<THREE.BufferGeometry>();
    const materials = new Set<THREE.Material>();
    scene.traverse((object) => {
      if (!(object instanceof THREE.Mesh || object instanceof THREE.Line)) return;
      geometries.add(object.geometry);
      const list = Array.isArray(object.material) ? object.material : [object.material];
      for (const material of list) materials.add(material);
    });
    for (const geometry of geometries) geometry.dispose();
    for (const material of materials) {
      (material as THREE.MeshBasicMaterial).map?.dispose();
      material.dispose();
    }
    scene.environment = null;
  }
  envTarget?.dispose();
  envTarget = null;
  if (renderer) {
    renderer.domElement.removeEventListener("webglcontextlost", onContextLost);
    renderer.domElement.removeEventListener("webglcontextrestored", onContextRestored);
    renderer.dispose();
    renderer.forceContextLoss();
    renderer.domElement.remove();
  }
  renderer = null;
  scene = null;
  viewCamera = null;
  panGroup = tiltGroup = lensGroup = fovGroup = null;
  footprint = null;
  footprintOutline = null;
  aimDot = null;
  fovFill = null;
  fovLine = null;
  statusLed = pointerMat = irMat = null;
  lastFrameTime = 0;
}

const hint = computed(() => (props.interactive ? "拖拽旋转视角 · 滚轮缩放 · 双击复位" : ""));

watch(
  () => [props.pan, props.tilt, props.zoom, props.active, props.seeking, props.online, props.showFov] as const,
  requestRender,
);
watch(() => props.interactive, (value) => {
  if (controls) controls.enabled = value;
});

onMounted(async () => {
  await nextTick();
  motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  reducedMotion = motionQuery.matches;
  motionQuery.addEventListener("change", onMotionPreference);
  buildScene();
});
onActivated(() => {
  isActiveView = true;
  requestRender();
});
onDeactivated(() => {
  isActiveView = false;
  if (animationFrame !== 0) cancelAnimationFrame(animationFrame);
  animationFrame = 0;
});
onUnmounted(disposeScene);
</script>

<template>
  <div ref="host" class="ptz-camera-3d" :class="{ interactive }" @dblclick="resetView">
    <div v-if="hint" class="hint">{{ hint }}</div>
    <div v-if="webglError" class="webgl-error">{{ webglError }}</div>
  </div>
</template>

<style scoped>
.ptz-camera-3d {
  position: absolute;
  inset: 0;
  z-index: 3;
  overflow: hidden;
  pointer-events: none;
}
.ptz-camera-3d.interactive { pointer-events: auto; cursor: grab; }
.ptz-camera-3d.interactive:active { cursor: grabbing; }
.ptz-camera-3d :deep(canvas) { display: block; width: 100%; height: 100%; outline: none; touch-action: none; }
.hint {
  position: absolute;
  left: 20px;
  bottom: 16px;
  color: rgba(200, 220, 244, 0.45);
  font-size: 11px;
  pointer-events: none;
  user-select: none;
}
.webgl-error {
  position: absolute;
  left: 50%;
  top: 50%;
  width: min(280px, calc(100% - 80px));
  transform: translate(-50%, -50%);
  padding: 10px 12px;
  border: 1px solid rgba(229, 72, 92, 0.25);
  border-radius: 10px;
  color: #b4233b;
  background: rgba(255, 255, 255, 0.92);
  font-size: 11px;
  text-align: center;
}
</style>
