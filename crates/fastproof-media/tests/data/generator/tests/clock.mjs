export const CLOCK = `<html><body style="margin:0"><canvas id="c" width="320" height="240"></canvas><script>
const g = document.getElementById("c").getContext("2d");
const t0 = performance.now();
function frame() {
  const t = performance.now() - t0;
  g.fillStyle = "hsl(" + ((t / 20) % 360) + ",70%,45%)";
  g.fillRect(0, 0, 320, 240);
  g.fillStyle = "#fff";
  g.font = "48px sans-serif";
  g.fillText((t / 1000).toFixed(1) + " s", 70, 140);
  requestAnimationFrame(frame);
}
frame();
</script></body></html>`;

