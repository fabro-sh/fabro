import { Composition } from "remotion";
import { FabroIntro } from "./FabroIntro";

export const RemotionRoot: React.FC = () => {
  return (
    <Composition
      id="FabroIntro"
      component={FabroIntro}
      durationInFrames={150}
      fps={30}
      width={1920}
      height={1080}
    />
  );
};
