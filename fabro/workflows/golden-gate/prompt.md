Use the /develop-web-game skill and the `imagegen` CLI tool.

Create a hyperrealistic interactive 3D experience of the San Francisco Golden Gate Bridge that the user can fly around freely. The environment should include realistic lighting, water, fog, atmosphere, suspension cables, traffic, surrounding coastline, and city context, with a cinematic sense of scale and detail. Let the user smoothly navigate through the scene with intuitive flight controls and multiple viewpoints, including close-up structural passes and wide scenic flyovers. Prioritize realism, immersion, and visual fidelity.

Use `imagegen` to generate source material and surface textures. For example:
```
imagegen "photorealistic golden gate bridge tower close-up, red steel, rivets, fog" output/textures/tower.png
imagegen "san francisco bay water surface, realistic ocean waves, sunlight reflections" output/textures/water.png
```

When playtesting with Playwright, fly around the bridge from multiple distances and angles, verify that navigation is smooth and stable, and confirm that the world looks convincing both up close and from afar. The result should look high fidelity and smooth, almost like a photo — not clunky or block-like. There should be realistic cars going over the bridge too.

Take your time and iterate until the experience is polished.
