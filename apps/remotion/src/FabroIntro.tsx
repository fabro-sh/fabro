import {
  AbsoluteFill,
  Img,
  interpolate,
  spring,
  useCurrentFrame,
  useVideoConfig,
  staticFile,
} from "remotion";

export const FabroIntro: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  // Symbol animation: scales up and fades in (frames 0-45)
  const symbolScale = spring({ frame, fps, from: 0.5, to: 1, durationInFrames: 45 });
  const symbolOpacity = interpolate(frame, [0, 20], [0, 1], { extrapolateRight: "clamp" });

  // Logotype slides up and fades in (frames 40-80)
  const logoOpacity = interpolate(frame, [40, 65], [0, 1], { extrapolateRight: "clamp" });
  const logoTranslateY = spring({ frame: Math.max(0, frame - 40), fps, from: 30, to: 0, durationInFrames: 30 });

  // Tagline fades in (frames 70-100)
  const taglineOpacity = interpolate(frame, [70, 95], [0, 1], { extrapolateRight: "clamp" });

  // Subtle background glow pulse
  const glowOpacity = interpolate(frame, [0, 75, 150], [0.3, 0.6, 0.3]);

  return (
    <AbsoluteFill
      style={{
        backgroundColor: "#0F1729",
        justifyContent: "center",
        alignItems: "center",
      }}
    >
      {/* Radial glow behind logo */}
      <div
        style={{
          position: "absolute",
          width: 800,
          height: 800,
          borderRadius: "50%",
          background:
            "radial-gradient(circle, rgba(103,178,215,0.25) 0%, rgba(15,23,41,0) 70%)",
          opacity: glowOpacity,
        }}
      />

      {/* Symbol */}
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 40,
        }}
      >
        <Img
          src={staticFile("symbol.svg")}
          style={{
            width: 200,
            opacity: symbolOpacity,
            transform: `scale(${symbolScale})`,
          }}
        />

        {/* Logotype */}
        <Img
          src={staticFile("logotype.svg")}
          style={{
            width: 500,
            opacity: logoOpacity,
            transform: `translateY(${logoTranslateY}px)`,
          }}
        />

        {/* Tagline */}
        <div
          style={{
            opacity: taglineOpacity,
            color: "#A8B5C5",
            fontSize: 32,
            fontFamily: "system-ui, -apple-system, sans-serif",
            fontWeight: 400,
            letterSpacing: 4,
            textTransform: "uppercase",
            marginTop: 10,
          }}
        >
          AI-Powered Workflow Orchestration
        </div>
      </div>
    </AbsoluteFill>
  );
};
