<script setup lang="ts">
import {
  nextTick,
  onActivated,
  onDeactivated,
  onMounted,
  onUnmounted,
  ref,
  watch,
} from "vue";
import * as THREE from "three";

const props = defineProps<{
  pan: number;
  tilt: number;
  zoom: number;
  active: boolean;
  seeking: boolean;
}>();

const host = ref<HTMLDivElement | null>(null);
const webglError = ref("");

let renderer: THREE.WebGLRenderer | null = null;
let scene: THREE.Scene | null = null;
let viewCamera: THREE.PerspectiveCamera | null = null;
let panGroup: THREE.Group | null = null;
let tiltGroup: THREE.Group | null = null;
let lensAssembly: THREE.Group | null = null;
let lensTube: THREE.Mesh<THREE.CylinderGeometry, THREE.MeshStandardMaterial> | null = null;
let beam: THREE.Mesh<THREE.CylinderGeometry, THREE.MeshBasicMaterial> | null = null;
let statusLight: THREE.Mesh<THREE.SphereGeometry, THREE.MeshStandardMaterial> | null = null;
let azimuthMarker: THREE.Mesh<THREE.BoxGeometry, THREE.MeshStandardMaterial> | null = null;
let resizeObserver: ResizeObserver | null = null;
let motionQuery: MediaQueryList | null = null;
let animationFrame = 0;
let isActiveView = true;
let reducedMotion = false;

const silver = new THREE.MeshStandardMaterial({ color: 0xd9e4ee, metalness: 0.72, roughness: 0.23 });
const darkSilver = new THREE.MeshStandardMaterial({ color: 0x71879f, metalness: 0.76, roughness: 0.26 });
const charcoal = new THREE.MeshStandardMaterial({ color: 0x111a28, metalness: 0.38, roughness: 0.3 });
const black = new THREE.MeshStandardMaterial({ color: 0x050a12, metalness: 0.2, roughness: 0.22 });
const blue = new THREE.MeshStandardMaterial({
  color: 0x2f8fff,
  emissive: 0x0d4fbc,
  emissiveIntensity: 1.25,
  metalness: 0.25,
  roughness: 0.2,
});
const glass = new THREE.MeshPhysicalMaterial({
  color: 0x168be0,
  emissive: 0x073e91,
  emissiveIntensity: 0.85,
  metalness: 0.12,
  roughness: 0.08,
  transmission: 0.22,
  thickness: 0.5,
  clearcoat: 1,
  clearcoatRoughness: 0.08,
});

function mesh<G extends THREE.BufferGeometry, M extends THREE.Material>(
  geometry: G,
  material: M,
  parent: THREE.Object3D,
): THREE.Mesh<G, M> {
  const value = new THREE.Mesh(geometry, material);
  value.castShadow = true;
  value.receiveShadow = true;
  parent.add(value);
  return value;
}

function addCylinder(
  parent: THREE.Object3D,
  radiusTop: number,
  radiusBottom: number,
  height: number,
  material: THREE.Material,
  y: number,
): THREE.Mesh {
  const value = mesh(new THREE.CylinderGeometry(radiusTop, radiusBottom, height, 48), material, parent);
  value.position.y = y;
  return value;
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
  renderer.toneMappingExposure = 1.08;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  renderer.domElement.setAttribute("aria-label", "Three.js 三维 PTZ 球机实时姿态");
  renderer.domElement.addEventListener("webglcontextlost", onContextLost);
  renderer.domElement.addEventListener("webglcontextrestored", onContextRestored);
  element.appendChild(renderer.domElement);

  scene = new THREE.Scene();
  viewCamera = new THREE.PerspectiveCamera(31, 1, 0.1, 100);
  viewCamera.position.set(5.7, 4.45, 8.1);
  viewCamera.lookAt(0, 1.35, 0);

  scene.add(new THREE.HemisphereLight(0xf6fbff, 0x60748e, 2.2));
  const key = new THREE.DirectionalLight(0xffffff, 4.3);
  key.position.set(4.5, 7, 6);
  key.castShadow = true;
  key.shadow.mapSize.set(1024, 1024);
  key.shadow.camera.near = 0.1;
  key.shadow.camera.far = 20;
  key.shadow.camera.left = -5;
  key.shadow.camera.right = 5;
  key.shadow.camera.top = 6;
  key.shadow.camera.bottom = -2;
  scene.add(key);
  const rim = new THREE.DirectionalLight(0x5599ff, 2.2);
  rim.position.set(-5, 3.5, -4);
  scene.add(rim);

  // Fixed desktop base without a decorative floor disc or dark bottom stripe.
  addCylinder(scene, 1.62, 1.78, 0.42, darkSilver, 0.21);
  addCylinder(scene, 1.52, 1.62, 0.34, silver, 0.49);
  const baseTrim = addCylinder(scene, 1.61, 1.61, 0.075, blue, 0.43);
  baseTrim.castShadow = false;

  // Everything above the fixed base follows the pan axis.
  panGroup = new THREE.Group();
  panGroup.position.y = 0.65;
  scene.add(panGroup);
  addCylinder(panGroup, 1.43, 1.5, 0.27, silver, 0.08);
  addCylinder(panGroup, 1.5, 1.5, 0.08, darkSilver, -0.02);
  const turntableTop = addCylinder(panGroup, 1.28, 1.39, 0.19, silver, 0.27);
  turntableTop.material = silver;

  azimuthMarker = mesh(new THREE.BoxGeometry(0.12, 0.07, 0.48), blue, panGroup);
  azimuthMarker.position.set(0, 0.41, 1.12);

  // U-shaped yoke rotates with the turntable but does not tilt.
  const bridge = mesh(new THREE.BoxGeometry(2.35, 0.34, 0.72), darkSilver, panGroup);
  bridge.position.set(0, 0.56, 0);
  bridge.geometry.translate(0, 0, 0);

  for (const side of [-1, 1]) {
    const arm = mesh(new THREE.CapsuleGeometry(0.2, 1.35, 8, 20), silver, panGroup);
    arm.position.set(side * 1.08, 1.34, 0);
    arm.rotation.z = side * -0.08;
    const hinge = mesh(new THREE.CylinderGeometry(0.3, 0.3, 0.22, 32), darkSilver, panGroup);
    hinge.position.set(side * 0.93, 1.72, 0);
    hinge.rotation.z = Math.PI / 2;
    const hingeCore = mesh(new THREE.CylinderGeometry(0.13, 0.13, 0.25, 24), blue, panGroup);
    hingeCore.position.copy(hinge.position);
    hingeCore.rotation.z = Math.PI / 2;
  }

  // The spherical camera head has its own physical tilt axis.
  tiltGroup = new THREE.Group();
  tiltGroup.position.set(0, 1.72, 0);
  panGroup.add(tiltGroup);

  const ball = mesh(new THREE.SphereGeometry(0.92, 64, 40), silver, tiltGroup);
  ball.scale.set(1, 0.96, 1);
  const facePlate = mesh(new THREE.CylinderGeometry(0.66, 0.72, 0.18, 48), charcoal, tiltGroup);
  facePlate.rotation.x = Math.PI / 2;
  facePlate.position.z = 0.78;

  // Fixed outer collar plus a long inner optical tube make zoom visibly telescope,
  // rather than scaling or translating the whole camera head.
  const lensSleeve = mesh(new THREE.CylinderGeometry(0.5, 0.57, 0.42, 48), charcoal, tiltGroup);
  lensSleeve.rotation.x = Math.PI / 2;
  lensSleeve.position.z = 0.93;
  const sleeveTrim = mesh(new THREE.TorusGeometry(0.48, 0.045, 12, 48), darkSilver, tiltGroup);
  sleeveTrim.position.z = 1.15;

  lensAssembly = new THREE.Group();
  lensAssembly.position.z = 1.02;
  tiltGroup.add(lensAssembly);
  lensTube = mesh(new THREE.CylinderGeometry(0.4, 0.44, 0.9, 48), black, lensAssembly);
  lensTube.rotation.x = Math.PI / 2;
  lensTube.position.z = -0.08;
  const barrelTrim = mesh(new THREE.TorusGeometry(0.4, 0.052, 12, 48), darkSilver, lensAssembly);
  barrelTrim.position.z = 0.39;
  const lens = mesh(new THREE.CylinderGeometry(0.33, 0.33, 0.09, 48), glass, lensAssembly);
  lens.rotation.x = Math.PI / 2;
  lens.position.z = 0.4;
  const lensInner = mesh(new THREE.CircleGeometry(0.19, 48), black, lensAssembly);
  lensInner.position.z = 0.448;
  const glint = mesh(new THREE.CircleGeometry(0.055, 24), new THREE.MeshBasicMaterial({ color: 0xc8f5ff }), lensAssembly);
  glint.position.set(-0.09, 0.1, 0.454);

  statusLight = mesh(
    new THREE.SphereGeometry(0.055, 20, 16),
    new THREE.MeshStandardMaterial({ color: 0xff4f5e, emissive: 0xff1f35, emissiveIntensity: 2.1 }),
    tiltGroup,
  );
  statusLight.position.set(0.48, 0.43, 0.77);
  for (const x of [-0.43, 0.43]) {
    const microphone = mesh(new THREE.SphereGeometry(0.035, 16, 12), black, tiltGroup);
    microphone.position.set(x, -0.39, 0.83);
  }

  // A real translucent 3D frustum shows the changing field of view.
  const beamMaterial = new THREE.MeshBasicMaterial({
    color: 0x3797ff,
    transparent: true,
    opacity: 0.1,
    depthWrite: false,
    side: THREE.DoubleSide,
    blending: THREE.AdditiveBlending,
  });
  beam = mesh(new THREE.CylinderGeometry(1.25, 0.12, 3.5, 48, 1, true), beamMaterial, tiltGroup);
  beam.rotation.x = Math.PI / 2;
  beam.position.z = 2.8;
  beam.castShadow = false;
  beam.receiveShadow = false;

  resizeObserver = new ResizeObserver(resize);
  resizeObserver.observe(element);
  resize();
  updatePose();
}

function resize(): void {
  if (!host.value || !renderer || !viewCamera) return;
  const width = Math.max(1, host.value.clientWidth);
  const height = Math.max(1, host.value.clientHeight);
  renderer.setSize(width, height, false);
  viewCamera.aspect = width / height;
  viewCamera.updateProjectionMatrix();
  renderScene();
}

function updatePose(): void {
  if (!panGroup || !tiltGroup || !lensAssembly || !lensTube || !beam || !statusLight || !azimuthMarker) return;
  panGroup.rotation.y = THREE.MathUtils.degToRad(props.pan);
  // The head faces +Z. Negative X rotation raises that vector, so positive semantic tilt is UP.
  tiltGroup.rotation.x = THREE.MathUtils.degToRad(-props.tilt);
  const zoomProgress = THREE.MathUtils.clamp((props.zoom - 1) / 3, 0, 1);
  const extension = zoomProgress * 0.52;
  lensAssembly.position.z = 1.02 + extension;
  lensTube.scale.y = 1 + zoomProgress * 0.14;
  const fieldScale = THREE.MathUtils.lerp(1.05, 0.46, zoomProgress);
  beam.position.z = 2.8 + extension;
  beam.scale.set(fieldScale, 1, fieldScale);
  beam.material.opacity = props.active || props.seeking ? 0.16 : 0.08;
  statusLight.material.color.setHex(props.active || props.seeking ? 0x2f8fff : 0x24c875);
  statusLight.material.emissive.setHex(props.active || props.seeking ? 0x0d63ff : 0x0b8f52);
  azimuthMarker.material.emissiveIntensity = props.active || props.seeking ? 2 : 1.25;
  requestRender();
}

function renderScene(): void {
  if (!isActiveView || !renderer || !scene || !viewCamera) return;
  renderer.render(scene, viewCamera);
}

function shouldAnimate(): boolean {
  return isActiveView && !reducedMotion && (props.active || props.seeking);
}

function animate(time: number): void {
  animationFrame = 0;
  if (!shouldAnimate() || !statusLight || !azimuthMarker) {
    renderScene();
    return;
  }
  const pulse = 1.45 + Math.sin(time * 0.008) * 0.65;
  statusLight.material.emissiveIntensity = pulse;
  azimuthMarker.material.emissiveIntensity = pulse;
  renderScene();
  animationFrame = requestAnimationFrame(animate);
}

function requestRender(): void {
  renderScene();
  if (shouldAnimate() && animationFrame === 0) {
    animationFrame = requestAnimationFrame(animate);
  }
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
  updatePose();
}

function disposeScene(): void {
  if (animationFrame !== 0) cancelAnimationFrame(animationFrame);
  animationFrame = 0;
  resizeObserver?.disconnect();
  resizeObserver = null;
  if (motionQuery) motionQuery.removeEventListener("change", onMotionPreference);
  motionQuery = null;
  if (scene) {
    scene.traverse((object) => {
      if (!(object instanceof THREE.Mesh)) return;
      object.geometry.dispose();
      const materials = Array.isArray(object.material) ? object.material : [object.material];
      for (const material of materials) material.dispose();
    });
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
  panGroup = null;
  tiltGroup = null;
  lensAssembly = null;
  lensTube = null;
  beam = null;
  statusLight = null;
  azimuthMarker = null;
}

watch(
  () => [props.pan, props.tilt, props.zoom, props.active, props.seeking] as const,
  updatePose,
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
  <div ref="host" class="three-ptz-camera">
    <div v-if="webglError" class="webgl-error">{{ webglError }}</div>
  </div>
</template>

<style scoped>
.three-ptz-camera {
  position: absolute;
  inset: 0;
  z-index: 4;
  overflow: hidden;
  pointer-events: none;
}
.three-ptz-camera :deep(canvas) {
  display: block;
  width: 100%;
  height: 100%;
  outline: none;
}
.webgl-error {
  position: absolute;
  left: 50%;
  top: 50%;
  width: min(260px, calc(100% - 80px));
  transform: translate(-50%, -50%);
  padding: 10px 12px;
  border: 1px solid rgba(229, 72, 92, 0.25);
  border-radius: 10px;
  color: #b4233b;
  background: rgba(255, 255, 255, 0.9);
  font-size: 11px;
  text-align: center;
}
</style>
