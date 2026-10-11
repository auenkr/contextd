"""Local JSON-lines MLX worker; bundled into the Rust executable."""
import json
import sys
from pathlib import Path

model = tokenizer = None

for line in sys.stdin:
    try:
        request = json.loads(line)
        action = request["action"]
        if action == "download":
            from huggingface_hub import snapshot_download
            destination = Path(request["destination"])
            snapshot_download(request["repository"], local_dir=str(destination),
                              allow_patterns=["*.json", "*.safetensors", "*.model", "*.jinja", "*.txt"])
            (destination / ".ready").write_text("complete\n")
            response = {"ok": True}
        elif action == "load":
            from mlx_lm import load
            model, tokenizer = load(request["model"])
            response = {"ok": True}
        elif action == "refine":
            from mlx_lm import stream_generate
            from mlx_lm.sample_utils import make_sampler
            prompt = tokenizer.apply_chat_template(request["messages"], tokenize=False,
                                                    add_generation_prompt=True, enable_thinking=False)
            pieces = []
            finish_reason = None
            for token in stream_generate(model, tokenizer, prompt=prompt,
                                         max_tokens=request["max_tokens"], sampler=make_sampler(temp=0.0)):
                pieces.append(token.text)
                finish_reason = token.finish_reason
            if finish_reason == "length":
                raise RuntimeError("MLX cleanup output truncated; original transcript preserved")
            response = {"ok": True, "text": "".join(pieces).strip()}
        else:
            raise ValueError(f"Unknown worker action: {action}")
    except Exception as error:
        response = {"ok": False, "error": f"{type(error).__name__}: {error}"}
    print(json.dumps(response, ensure_ascii=False), flush=True)
