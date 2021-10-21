import { start } from "wrath";

const main = async () => {
  const content = document.getElementById("content");
  await start(content);
}

main();
