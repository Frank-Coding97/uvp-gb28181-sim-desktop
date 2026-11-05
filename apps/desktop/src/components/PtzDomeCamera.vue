<script setup lang="ts">
// 室外变速球机 3D 模型（重画版）。
//
// 设计目标：这是一台“相机”，不是会转的机器人。
// - 固定部分：铸铝底座 + 防水胶圈 + 进线防水接头 + 锥形立柱 + 回转轴承；
// - 转动部分：圆柱机身 + 遮阳罩 + 烟灰球罩 + 内部云台/镜头；
// - 材质：粉末喷涂铝、阳极氧化深灰、烟灰玻璃、橡胶、不锈钢螺钉；
// - 光照：环境贴图 + 主光/轮廓光/补光 + 接触阴影，产品级展示。
//
// 这个组件只表达“设备姿态”，不接视频流。pan 转整机，tilt 转内部云台，
// zoom 驱动镜头前伸；另外可选自检动画和 FOV 光锥（默认关闭）。
import { nextTick, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";
import * as THREE from "three";
import { RoomEnvironment } from "three/addons/environments/RoomEnvironment.js";

const props = withDefaults(defineProps<{
  pan: number;
  tilt: number;
  zoom: number;
  active?: boolean;
  seeking?: boolean;
  guarded?: boolean;
  selfTest?: boolean;
  showFovCone?: boolean;
}>(), {
  active: false,
  seeking: false,
  guarded: false,
  selfTest: false,
  showFovCone: false,
});

const host = ref<HTMLDivElement | null>(null);
const webglError = ref("");

let renderer: THREE.WebGLRenderer | null = null;
let scene: THREE.Scene | null = null;
let viewCamera: THREE.PerspectiveCamera | null = null;
let turret: THREE.Group | null = null;
let innerPan: THREE.Group | null = null;
let tiltGroup: THREE.Group | null = null;
let lensGroup: THREE.Group | null = null;
let fovCone: THREE.Mesh | null = null;
let guardRing: THREE.Mesh | null = null;
let ledMaterial: THREE.MeshStandardMaterial | null = null;
let guardMaterial: THREE.MeshBasicMaterial | null = null;
let envTarget: THREE.WebGLRenderTarget | null = null;
let environmentScene: THREE.Scene | null = null;
let resizeObserver: ResizeObserver | null = null;
let motionQuery: MediaQueryList | null = null;
let animationFrame = 0;
let isActiveView = true;
let reducedMotion = false;
let lastFrameTime = 0;

let displayPan = 0;
let displayTilt = 0;
let displayZoom = 1;
let guardAlpha = 0;
let selfTestActive = false;
let selfTestStartedAt = 0;

const ZOOM_MAX = 20;

type Materials = ReturnType<typeof createMaterials>;

function createMaterials() {
  const powder = new THREE.MeshStandardMaterial({
    color: 0x9aa4ae,
    metalness: 0.18,
    roughness: 0.5,
  });
  const powderDark = new THREE.MeshStandardMaterial({
    color: 0x434a53,
    metalness: 0.28,
    roughness: 0.44,
  });
  const anodized = new THREE.MeshStandardMaterial({
    color: 0x171c22,
    metalness: 0.68,
    roughness: 0.3,
  });
  const rubber = new THREE.MeshStandardMaterial({
    color: 0x0c0e11,
    metalness: 0,
    roughness: 0.94,
  });
  const steel = new THREE.MeshStandardMaterial({
    color: 0xaeb8c2,
    metalness: 0.92,
    roughness: 0.24,
  });
  const brass = new THREE.MeshStandardMaterial({
    color: 0xb8893f,
    metalness: 0.9,
    roughness: 0.32,
  });
  const glass = new THREE.MeshPhysicalMaterial({
    color: 0x52626f,
    metalness: 0,
    roughness: 0.06,
    transmission: 0.38,
    thickness: 0.3,
    ior: 1.46,
    transparent: true,
    opacity: 0.58,
    clearcoat: 1,
    clearcoatRoughness: 0.04,
    side: THREE.DoubleSide,
    depthWrite: false,
  });
  const lens = new THREE.MeshPhysicalMaterial({
    color: 0x050b12,
    metalness: 0.25,
    roughness: 0.03,
    clearcoat: 1,
    clearcoatRoughness: 0.02,
    ior: 1.5,
  });
  const led = new THREE.MeshStandardMaterial({
    color: 0x2f8fff,
    emissive: 0x1d6dff,
    emissiveIntensity: 1.2,
    metalness: 0.25,
    roughness: 0.3,
  });
  const guard = new THREE.MeshBasicMaterial({
    color: 0x2ad38b,
    transparent: true,
    opacity: 0,
    depthWrite: false,
    blending: THREE.AdditiveBlending,
  });

  return { powder, powderDark, anodized, rubber, steel, brass, glass, lens, led, guard };
}

function lathe(points: Array<[number, number]>, segments = 64): THREE.LatheGeometry {
  const geometry = new THREE.LatheGeometry(
    points.map(([x, y]) => new THREE.Vector2(x, y)),
    segments,
  );
  geometry.computeVertexNormals();
  return geometry;
}

function addMesh(
  parent: THREE.Object3D,
  geometry: THREE.BufferGeometry,
  material: THREE.Material,
  options: { cast?: boolean; receive?: boolean } = {},
): THREE.Mesh {
  const mesh = new THREE.Mesh(geometry, material);
  mesh.castShadow = options.cast ?? true;
  mesh.receiveShadow = options.receive ?? true;
  parent.add(mesh);
  return mesh;
}

function addScrew(parent: THREE.Object3D, x: number, y: number, z: number, radius = 0.055): void {
  const steel = getMaterialSet();
  const head = addMesh(parent, new THREE.CylinderGeometry(radius, radius, 0.024, 6), steel.steel);
  head.position.set(x, y, z);
  const socket = addMesh(
    parent,
    new THREE.CylinderGeometry(radius * 0.55, radius * 0.55, 0.007, 6),
    steel.anodized,
    { cast: false },
  );
  socket.position.set(x, y + 0.014, z);
}

// 构建期临时引用；只在 buildScene 内使用，避免把材质作为模块级单例（HMR 会复用已销毁材质）。
let activeMaterials: Materials | null = null;
function getMaterialSet(): Materials {
  if (!activeMaterials) throw new Error("材质尚未初始化");
  return activeMaterials;
}

function buildMount(mats: Materials): void {
  const fixed = new THREE.Group();
  fixed.name = "desk-stand";
  scene?.add(fixed);

  // 桌面底座：椭圆形压铸底盘，前低后高，给后方立柱留出落脚点。
  const base = addMesh(fixed, lathe([
    [0, 0],
    [0.74, 0],
    [0.86, 0.03],
    [0.9, 0.09],
    [0.86, 0.14],
    [0.74, 0.16],
    [0, 0.16],
  ]), mats.powderDark);
  base.scale.z = 1.34;

  const gasket = addMesh(fixed, new THREE.TorusGeometry(0.78, 0.026, 10, 48), mats.rubber, {
    cast: false,
    receive: false,
  });
  gasket.rotation.x = Math.PI / 2;
  gasket.position.y = 0.155;
  gasket.scale.z = 1.34;

  for (let i = 0; i < 3; i += 1) {
    const angle = (i / 3) * Math.PI * 2 + Math.PI / 6;
    addScrew(fixed, Math.cos(angle) * 0.62, 0.165, Math.sin(angle) * 0.83, 0.04);
  }

  // 后方立柱 + 横臂：从底座伸到机身背面，不穿过球罩。
  const post = addMesh(fixed, new THREE.CylinderGeometry(0.085, 0.105, 1.48, 24), mats.powder);
  post.position.set(0, 0.9, -1.02);

  const elbow = addMesh(fixed, new THREE.SphereGeometry(0.15, 24, 16), mats.powder);
  elbow.position.set(0, 1.62, -1.02);

  const arm = addMesh(fixed, new THREE.CylinderGeometry(0.075, 0.075, 0.24, 20), mats.powder);
  arm.rotation.x = Math.PI / 2;
  arm.position.set(0, 1.62, -0.9);

  // 机身背面的安装座。
  const mountBoss = addMesh(fixed, new THREE.CylinderGeometry(0.17, 0.17, 0.16, 32), mats.anodized);
  mountBoss.rotation.x = Math.PI / 2;
  mountBoss.position.set(0, 1.62, -0.92);

  // 底座后方的出线防水接头。
  const glandNut = addMesh(fixed, new THREE.CylinderGeometry(0.09, 0.09, 0.07, 6), mats.steel);
  glandNut.rotation.x = Math.PI / 2;
  glandNut.position.set(0, 0.14, -1.18);
  const gland = addMesh(fixed, new THREE.CylinderGeometry(0.06, 0.06, 0.18, 20), mats.anodized);
  gland.rotation.x = Math.PI / 2;
  gland.position.set(0, 0.14, -1.3);
}

function buildTurret(mats: Materials): void {
  turret = new THREE.Group();
  turret.name = "turret";
  turret.position.y = 1.45;
  scene?.add(turret);

  // 回转轴承：藏在球罩上沿，尺寸比机身小。
  const bearing = addMesh(turret, new THREE.CylinderGeometry(0.52, 0.58, 0.08, 48), mats.anodized);
  bearing.position.y = 0.02;

  // 深灰圆柱机身：下沿与球罩齐平，上沿收肩。
  addMesh(turret, lathe([
    [0.96, 0.0],
    [1.0, 0.04],
    [1.0, 0.38],
    [0.98, 0.5],
    [0.9, 0.55],
    [0, 0.55],
  ]), mats.powderDark);

  // 烟灰球罩：固定球罩，内部云台在后面动。
  const dome = addMesh(
    turret,
    new THREE.SphereGeometry(0.95, 64, 40, 0, Math.PI * 2, Math.PI / 2, Math.PI * 0.42),
    mats.glass,
    { cast: false, receive: false },
  );
  dome.position.y = 0;
  dome.renderOrder = 4;

  const domeRim = addMesh(turret, new THREE.TorusGeometry(0.95, 0.028, 12, 64), mats.steel);
  domeRim.rotation.x = Math.PI / 2;
  domeRim.position.y = 0;

  // 遮阳罩 / 顶盖：和机身一体，前沿只比机身多出一点。
  addMesh(turret, lathe([
    [0, 0.5],
    [0.84, 0.5],
    [0.96, 0.53],
    [1.01, 0.58],
    [0.98, 0.62],
    [0.82, 0.64],
    [0, 0.66],
  ]), mats.powderDark);

  const shieldLip = addMesh(turret, new THREE.TorusGeometry(1.0, 0.014, 8, 64), mats.steel, {
    cast: false,
  });
  shieldLip.rotation.x = Math.PI / 2;
  shieldLip.position.y = 0.58;

  // 遮阳罩下沿的蓝色状态光环。
  const ledRing = addMesh(turret, new THREE.TorusGeometry(0.99, 0.014, 8, 64), mats.led, {
    cast: false,
    receive: false,
  });
  ledRing.rotation.x = Math.PI / 2;
  ledRing.position.y = 0.49;

  const statusLight = addMesh(turret, new THREE.SphereGeometry(0.038, 16, 12), mats.led, {
    cast: false,
    receive: false,
  });
  statusLight.position.set(0, 0.33, 0.995);
  ledMaterial = mats.led;

  // 机身铭牌（CanvasTexture，不需要外部资源）。
  const label = addMesh(
    turret,
    new THREE.PlaneGeometry(0.46, 0.1),
    new THREE.MeshBasicMaterial({
      map: createLabelTexture(),
      transparent: true,
      depthWrite: false,
      polygonOffset: true,
      polygonOffsetFactor: -1,
    }),
    { cast: false, receive: false },
  );
  label.position.set(0, 0.2, 1.006);

  // 布防状态环：贴在球罩上沿，随转台一起转。
  guardRing = addMesh(turret, new THREE.TorusGeometry(1.0, 0.012, 10, 64), mats.guard, {
    cast: false,
    receive: false,
  });
  guardRing.rotation.x = Math.PI / 2;
  guardRing.position.y = 0.008;
  guardMaterial = mats.guard;
}

function buildInnerCamera(mats: Materials): void {
  if (!turret) return;

  // 内部云台：外罩固定，只有这一组在球罩里做水平/俯仰。
  innerPan = new THREE.Group();
  innerPan.position.set(0, -0.02, 0);
  turret.add(innerPan);

  const hub = addMesh(innerPan, new THREE.CylinderGeometry(0.16, 0.16, 0.14, 32), mats.anodized);
  hub.position.y = 0.04;

  for (const side of [-1, 1]) {
    const arm = addMesh(innerPan, new THREE.CylinderGeometry(0.035, 0.035, 0.34, 16), mats.anodized);
    arm.position.set(side * 0.27, -0.12, 0);
    const pivot = addMesh(innerPan, new THREE.CylinderGeometry(0.062, 0.062, 0.055, 20), mats.steel);
    pivot.rotation.z = Math.PI / 2;
    pivot.position.set(side * 0.27, -0.28, 0);
  }

  tiltGroup = new THREE.Group();
  tiltGroup.position.set(0, -0.06, 0);
  innerPan.add(tiltGroup);

  const body = addMesh(tiltGroup, new THREE.CapsuleGeometry(0.22, 0.34, 8, 24), mats.anodized);
  body.rotation.x = Math.PI / 2;
  body.position.set(0, -0.16, 0.02);

  const bodyTop = addMesh(tiltGroup, new THREE.CylinderGeometry(0.19, 0.23, 0.09, 32), mats.powder);
  bodyTop.position.set(0, 0.03, 0.02);

  const bodyBand = addMesh(tiltGroup, new THREE.TorusGeometry(0.225, 0.012, 8, 32), mats.steel);
  bodyBand.position.set(0, -0.16, 0.16);

  // 镜头组整体前后移动，表达变倍。
  lensGroup = new THREE.Group();
  tiltGroup.add(lensGroup);

  const barrel = addMesh(lensGroup, new THREE.CylinderGeometry(0.16, 0.19, 0.24, 40), mats.anodized);
  barrel.rotation.x = Math.PI / 2;
  barrel.position.set(0, -0.16, 0.48);

  const barrelRing = addMesh(lensGroup, new THREE.TorusGeometry(0.172, 0.017, 10, 40), mats.steel);
  barrelRing.position.set(0, -0.16, 0.6);

  const glass = addMesh(lensGroup, new THREE.CircleGeometry(0.15, 40), mats.lens, {
    cast: false,
    receive: false,
  });
  glass.position.set(0, -0.16, 0.607);

  const iris = addMesh(
    lensGroup,
    new THREE.CircleGeometry(0.09, 32),
    new THREE.MeshBasicMaterial({ color: 0x02060b }),
    { cast: false, receive: false },
  );
  iris.position.set(0, -0.16, 0.609);

  const glint = addMesh(
    lensGroup,
    new THREE.CircleGeometry(0.018, 20),
    new THREE.MeshBasicMaterial({ color: 0xbde9ff }),
    { cast: false, receive: false },
  );
  glint.position.set(-0.045, -0.12, 0.61);

  for (let i = 0; i < 8; i += 1) {
    const angle = (i / 8) * Math.PI * 2;
    const led = addMesh(
      lensGroup,
      new THREE.SphereGeometry(0.011, 10, 8),
      new THREE.MeshStandardMaterial({
        color: 0x2a0505,
        emissive: 0x4a0707,
        emissiveIntensity: 0.5,
        roughness: 0.4,
      }),
      { cast: false, receive: false },
    );
    led.position.set(Math.cos(angle) * 0.19, -0.16 + Math.sin(angle) * 0.19, 0.592);
  }

  // FOV 光锥：默认隐藏，仅在调试开关打开时显示。
  fovCone = addMesh(
    lensGroup,
    new THREE.CylinderGeometry(0.9, 0.12, 2.4, 32, 1, true),
    new THREE.MeshBasicMaterial({
      color: 0x4aa3ff,
      transparent: true,
      opacity: 0.085,
      depthWrite: false,
      side: THREE.DoubleSide,
      blending: THREE.AdditiveBlending,
    }),
    { cast: false, receive: false },
  );
  fovCone.rotation.x = Math.PI / 2;
  fovCone.position.set(0, -0.16, 1.45);
  fovCone.visible = false;
  fovCone.renderOrder = 2;
}

function createLabelTexture(): THREE.CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = 512;
  canvas.height = 128;
  const ctx = canvas.getContext("2d");
  if (ctx) {
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.fillStyle = "rgba(214, 222, 230, 0.96)";
    ctx.beginPath();
    ctx.roundRect(4, 4, 504, 120, 18);
    ctx.fill();
    ctx.strokeStyle = "rgba(24, 31, 39, 0.42)";
    ctx.lineWidth = 2;
    ctx.stroke();
    ctx.fillStyle = "#1b222a";
    ctx.font = "700 54px 'SF Pro Display', 'Segoe UI', sans-serif";
    ctx.fillText("UVP", 32, 82);
    ctx.fillStyle = "#394654";
    ctx.font = "600 30px 'SF Pro Display', 'Segoe UI', sans-serif";
    ctx.fillText("PTZ", 186, 80);
    ctx.fillStyle = "#5d6c7a";
    ctx.font = "500 18px 'SF Mono', 'Consolas', monospace";
    ctx.fillText("GB/T 28181", 300, 78);
  }
  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  texture.anisotropy = 4;
  return texture;
}

function createContactShadow(): THREE.Mesh {
  const canvas = document.createElement("canvas");
  canvas.width = 256;
  canvas.height = 256;
  const ctx = canvas.getContext("2d");
  if (ctx) {
    const gradient = ctx.createRadialGradient(128, 128, 8, 128, 128, 120);
    gradient.addColorStop(0, "rgba(0, 0, 0, 0.46)");
    gradient.addColorStop(0.55, "rgba(0, 0, 0, 0.18)");
    gradient.addColorStop(1, "rgba(0, 0, 0, 0)");
    ctx.fillStyle = gradient;
    ctx.fillRect(0, 0, 256, 256);
  }
  const texture = new THREE.CanvasTexture(canvas);
  const material = new THREE.MeshBasicMaterial({
    map: texture,
    transparent: true,
    depthWrite: false,
    opacity: 0.62,
  });
  const mesh = new THREE.Mesh(new THREE.PlaneGeometry(5.4, 5.4), material);
  mesh.rotation.x = -Math.PI / 2;
  mesh.position.y = 0.003;
  mesh.renderOrder = 0;
  return mesh;
}

function buildScene(): void {
  const element = host.value;
  if (!element) return;

  try {
    renderer = new THREE.WebGLRenderer({
      antialias: true,
      alpha: true,
      powerPreference: "high-performance",
    });
  } catch (error) {
    webglError.value = `WebGL 初始化失败：${String(error)}`;
    return;
  }

  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.0;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  renderer.domElement.setAttribute("aria-label", "室外变速球机三维姿态");
  renderer.domElement.addEventListener("webglcontextlost", onContextLost);
  renderer.domElement.addEventListener("webglcontextrestored", onContextRestored);
  element.appendChild(renderer.domElement);

  scene = new THREE.Scene();
  viewCamera = new THREE.PerspectiveCamera(31, 1, 0.1, 100);

  // 环境贴图：没有它，金属和球罩都会发灰。
  const pmrem = new THREE.PMREMGenerator(renderer);
  environmentScene = new RoomEnvironment();
  envTarget = pmrem.fromScene(environmentScene, 0.04);
  scene.environment = envTarget.texture;
  scene.environmentIntensity = 0.55;
  pmrem.dispose();

  const hemisphere = new THREE.HemisphereLight(0xe8f3ff, 0x0a111a, 0.38);
  scene.add(hemisphere);

  const key = new THREE.DirectionalLight(0xfff6ea, 2.6);
  key.position.set(4.6, 5.6, 6.2);
  key.castShadow = true;
  key.shadow.mapSize.set(2048, 2048);
  key.shadow.camera.near = 0.5;
  key.shadow.camera.far = 20;
  key.shadow.camera.left = -4;
  key.shadow.camera.right = 4;
  key.shadow.camera.top = 6;
  key.shadow.camera.bottom = -2;
  key.shadow.bias = -0.0004;
  key.shadow.normalBias = 0.02;
  scene.add(key);

  const rim = new THREE.DirectionalLight(0x6fa8ff, 1.7);
  rim.position.set(-5.2, 3.8, -4.2);
  scene.add(rim);

  const fill = new THREE.DirectionalLight(0xbcd6ff, 0.45);
  fill.position.set(-2.4, 2.2, 5.4);
  scene.add(fill);

  // 球罩内部的一点点补光，让内部云台能看出轮廓。
  const innerFill = new THREE.PointLight(0x9ec8ff, 0.45, 1.8);
  innerFill.position.set(0, 1.35, 0.35);
  scene.add(innerFill);

  const floor = new THREE.Mesh(
    new THREE.PlaneGeometry(40, 40),
    new THREE.ShadowMaterial({ opacity: 0.32 }),
  );
  floor.rotation.x = -Math.PI / 2;
  floor.position.y = 0;
  floor.receiveShadow = true;
  scene.add(floor);

  scene.add(createContactShadow());

  activeMaterials = createMaterials();
  buildMount(activeMaterials);
  buildTurret(activeMaterials);
  buildInnerCamera(activeMaterials);

  // 初始姿态：直接落位，避免首帧从 0 缓动到当前值。
  displayPan = props.pan;
  displayTilt = props.tilt;
  displayZoom = props.zoom;
  applyPose();

  resizeObserver = new ResizeObserver(resize);
  resizeObserver.observe(element);
  resize();

  if (props.selfTest) startSelfTest();
}

function applyPose(): void {
  if (!turret || !innerPan || !tiltGroup || !lensGroup) return;
  innerPan.rotation.y = THREE.MathUtils.degToRad(displayPan);
  tiltGroup.rotation.x = THREE.MathUtils.degToRad(-displayTilt);

  const zoomProgress = THREE.MathUtils.clamp(
    Math.log(Math.max(1, displayZoom)) / Math.log(ZOOM_MAX),
    0,
    1,
  );
  lensGroup.position.z = zoomProgress * 0.18;

  if (fovCone) {
    fovCone.visible = props.showFovCone;
    if (props.showFovCone) {
      const scale = THREE.MathUtils.lerp(1.05, 0.42, zoomProgress);
      fovCone.scale.set(scale, 1, scale);
      fovCone.position.z = 1.45 + zoomProgress * 0.18;
    }
  }
}

function updatePose(dt: number, time: number): boolean {
  if (!turret || !innerPan || !tiltGroup || !lensGroup) return false;

  let targetPan = props.pan;
  let targetTilt = props.tilt;
  let targetZoom = props.zoom;

  if (selfTestActive) {
    if (selfTestStartedAt === 0) selfTestStartedAt = time;
    const sample = selfTestSample((time - selfTestStartedAt) / 1000);
    targetPan = sample.pan;
    targetTilt = sample.tilt;
    targetZoom = 1;
    if (sample.done) selfTestActive = false;
  }

  const poseEase = reducedMotion ? 1 : 1 - Math.exp(-dt * 9);
  const zoomEase = reducedMotion ? 1 : 1 - Math.exp(-dt * 7);
  const panDelta = ((targetPan - displayPan + 540) % 360) - 180;
  const tiltDelta = THREE.MathUtils.clamp(targetTilt, -90, 90) - displayTilt;
  const zoomDelta = THREE.MathUtils.clamp(targetZoom, 1, ZOOM_MAX) - displayZoom;

  displayPan += panDelta * poseEase;
  displayTilt += tiltDelta * poseEase;
  displayZoom += zoomDelta * zoomEase;

  innerPan.rotation.y = THREE.MathUtils.degToRad(displayPan);
  tiltGroup.rotation.x = THREE.MathUtils.degToRad(-displayTilt);

  const zoomProgress = THREE.MathUtils.clamp(
    Math.log(Math.max(1, displayZoom)) / Math.log(ZOOM_MAX),
    0,
    1,
  );
  lensGroup.position.z = zoomProgress * 0.18;

  if (fovCone) {
    fovCone.visible = props.showFovCone;
    if (props.showFovCone) {
      const scale = THREE.MathUtils.lerp(1.05, 0.42, zoomProgress);
      fovCone.scale.set(scale, 1, scale);
      fovCone.position.z = 1.45 + zoomProgress * 0.18;
    }
  }

  const lit = props.active || props.seeking;
  if (ledMaterial) {
    const pulse = reducedMotion ? 1.1 : 1.15 + Math.sin(time * 0.006) * 0.45;
    ledMaterial.color.setHex(lit ? 0x2f8fff : 0x2ad38b);
    ledMaterial.emissive.setHex(lit ? 0x1d6dff : 0x0d8a5a);
    ledMaterial.emissiveIntensity = lit ? pulse : 1.0;
  }

  const guardTarget = props.guarded ? 1 : 0;
  guardAlpha += (guardTarget - guardAlpha) * (reducedMotion ? 1 : 1 - Math.exp(-dt * 6));
  if (guardMaterial) guardMaterial.opacity = guardAlpha * 0.8;

  const stillMoving =
    Math.abs(panDelta) > 0.04 ||
    Math.abs(tiltDelta) > 0.04 ||
    Math.abs(zoomDelta) > 0.005 ||
    selfTestActive ||
    lit ||
    Math.abs(guardTarget - guardAlpha) > 0.005;

  return stillMoving;
}

function selfTestSample(t: number): { pan: number; tilt: number; done: boolean } {
  const panAmp = 50;
  const tiltAmp = 25;
  if (t < 0.5) return { pan: 0, tilt: 0, done: false };
  if (t < 1.5) return { pan: ease(0, -panAmp, (t - 0.5) / 1), tilt: 0, done: false };
  if (t < 2.5) return { pan: ease(-panAmp, panAmp, (t - 1.5) / 1), tilt: 0, done: false };
  if (t < 3.0) return { pan: ease(panAmp, 0, (t - 2.5) / 0.5), tilt: 0, done: false };
  if (t < 3.5) return { pan: 0, tilt: 0, done: false };
  if (t < 4.5) return { pan: 0, tilt: ease(0, tiltAmp, (t - 3.5) / 1), done: false };
  if (t < 5.5) return { pan: 0, tilt: ease(tiltAmp, -tiltAmp, (t - 4.5) / 1), done: false };
  if (t < 6.0) return { pan: 0, tilt: ease(-tiltAmp, 0, (t - 5.5) / 0.5), done: false };
  return { pan: 0, tilt: 0, done: true };
}

function ease(from: number, to: number, phase: number): number {
  const p = THREE.MathUtils.clamp(phase, 0, 1);
  const eased = 0.5 - 0.5 * Math.cos(p * Math.PI);
  return from + (to - from) * eased;
}

function startSelfTest(): void {
  selfTestActive = true;
  selfTestStartedAt = 0;
  requestRender();
}

function resize(): void {
  if (!host.value || !renderer || !viewCamera) return;
  const width = Math.max(1, host.value.clientWidth);
  const height = Math.max(1, host.value.clientHeight);
  renderer.setSize(width, height, false);

  const aspect = width / height;
  viewCamera.aspect = aspect;
  viewCamera.fov = aspect < 1.12 ? 35 : 31;
  viewCamera.position.set(3.5, 2.35, 5.0);
  if (aspect < 1.12) viewCamera.position.multiplyScalar(1.16);
  viewCamera.lookAt(0, 1.1, 0);
  viewCamera.updateProjectionMatrix();
  renderScene();
}

function renderScene(): void {
  if (!isActiveView || !renderer || !scene || !viewCamera) return;
  renderer.render(scene, viewCamera);
}

function renderLoop(time: number): void {
  animationFrame = 0;
  if (!renderer || !scene || !viewCamera) return;
  const dt = lastFrameTime === 0 ? 0.016 : Math.min(0.05, (time - lastFrameTime) / 1000);
  lastFrameTime = time;
  const moving = updatePose(dt, time);
  renderScene();
  if (moving && isActiveView) {
    animationFrame = requestAnimationFrame(renderLoop);
  }
}

function requestRender(): void {
  if (!isActiveView) return;
  if (animationFrame === 0) animationFrame = requestAnimationFrame(renderLoop);
}

function onMotionPreference(event: MediaQueryListEvent | MediaQueryList): void {
  reducedMotion = event.matches;
  if (reducedMotion && animationFrame !== 0) {
    cancelAnimationFrame(animationFrame);
    animationFrame = 0;
  }
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
  requestRender();
}

function disposeSceneGraph(root: THREE.Object3D): void {
  const geometries = new Set<THREE.BufferGeometry>();
  const materials = new Set<THREE.Material>();
  root.traverse((object) => {
    if (!(object instanceof THREE.Mesh)) return;
    geometries.add(object.geometry);
    const list = Array.isArray(object.material) ? object.material : [object.material];
    for (const material of list) materials.add(material);
  });
  for (const geometry of geometries) geometry.dispose();
  for (const material of materials) {
    const mapped = material as THREE.MeshStandardMaterial;
    mapped.map?.dispose();
    material.dispose();
  }
}

function disposeScene(): void {
  if (animationFrame !== 0) cancelAnimationFrame(animationFrame);
  animationFrame = 0;
  resizeObserver?.disconnect();
  resizeObserver = null;
  if (motionQuery) motionQuery.removeEventListener("change", onMotionPreference);
  motionQuery = null;

  if (scene) {
    disposeSceneGraph(scene);
    scene.environment = null;
  }
  envTarget?.dispose();
  envTarget = null;
  if (environmentScene) {
    disposeSceneGraph(environmentScene);
    environmentScene = null;
  }
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
  turret = null;
  innerPan = null;
  tiltGroup = null;
  lensGroup = null;
  fovCone = null;
  guardRing = null;
  ledMaterial = null;
  guardMaterial = null;
  activeMaterials = null;
  lastFrameTime = 0;
}

watch(
  () => [props.pan, props.tilt, props.zoom, props.active, props.seeking, props.guarded, props.showFovCone] as const,
  () => requestRender(),
);

watch(
  () => props.selfTest,
  (value) => {
    if (value) startSelfTest();
    else {
      selfTestActive = false;
      requestRender();
    }
  },
);

onMounted(async () => {
  await nextTick();
  motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  onMotionPreference(motionQuery);
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
  <div ref="host" class="ptz-dome-camera">
    <div v-if="webglError" class="webgl-error">{{ webglError }}</div>
  </div>
</template>

<style scoped>
.ptz-dome-camera {
  position: absolute;
  inset: 0;
  z-index: 2;
  overflow: hidden;
  pointer-events: none;
}
.ptz-dome-camera :deep(canvas) {
  display: block;
  width: 100%;
  height: 100%;
  outline: none;
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
