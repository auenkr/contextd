"""JSON-line local diarization/voice embedding worker; no audio leaves this process."""
import json
import os
from pathlib import Path
import sys
import tarfile
import ssl
import certifi
import urllib.request
import wave
import numpy as np
import sherpa_onnx

CACHE = Path(sys.argv[1])
CACHE.mkdir(parents=True, exist_ok=True)
SEGMENTATION = CACHE / 'segmentation.onnx'
EMBEDDING = CACHE / 'wespeaker_en_voxceleb_resnet34.onnx'
BASE = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/'


def download(url, destination):
    temporary = destination.with_suffix('.part')
    try:
        with urllib.request.urlopen(url, timeout=120, context=ssl.create_default_context(cafile=certifi.where())) as response, temporary.open('wb') as output:
            while chunk := response.read(1024 * 1024):
                output.write(chunk)
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def load():
    if not SEGMENTATION.exists():
        archive = CACHE / 'segmentation.tar.bz2'
        download(BASE + 'speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2', archive)
        try:
            with tarfile.open(archive) as package:
                member = package.getmember('sherpa-onnx-pyannote-segmentation-3-0/model.onnx')
                if not member.isfile():
                    raise ValueError('Segmentation archive contains no regular model file')
                temporary = SEGMENTATION.with_suffix('.part')
                with package.extractfile(member) as source, temporary.open('wb') as output:
                    while chunk := source.read(1024 * 1024):
                        output.write(chunk)
                os.replace(temporary, SEGMENTATION)
        finally:
            archive.unlink(missing_ok=True)
    if not EMBEDDING.exists():
        download(BASE + 'speaker-recongition-models/' + EMBEDDING.name, EMBEDDING)
    embedding_config = sherpa_onnx.SpeakerEmbeddingExtractorConfig(model=str(EMBEDDING), num_threads=2)
    if not embedding_config.validate():
        raise ValueError('Invalid speaker embedding model')
    extractor = sherpa_onnx.SpeakerEmbeddingExtractor(embedding_config)
    config = sherpa_onnx.OfflineSpeakerDiarizationConfig(
        segmentation=sherpa_onnx.OfflineSpeakerSegmentationModelConfig(
            pyannote=sherpa_onnx.OfflineSpeakerSegmentationPyannoteModelConfig(model=str(SEGMENTATION))),
        embedding=embedding_config,
        clustering=sherpa_onnx.FastClusteringConfig(num_clusters=-1, threshold=0.5),
        min_duration_on=0.3, min_duration_off=0.5)
    if not config.validate():
        raise ValueError('Invalid speaker segmentation model')
    return sherpa_onnx.OfflineSpeakerDiarization(config), extractor


engines = None
for line in sys.stdin:
    try:
        request = json.loads(line)
        if engines is None:
            engines = load()
        diarizer, extractor = engines
        with wave.open(request['audio']) as audio:
            if audio.getframerate() != 16000 or audio.getnchannels() != 1 or audio.getsampwidth() != 2:
                raise ValueError('Speaker input must be mono PCM16 at 16 kHz')
            samples = np.frombuffer(audio.readframes(audio.getnframes()), dtype='<i2').astype(np.float32) / 32768
        results = diarizer.process(samples).sort_by_start_time()
        turns = [{'start':float(r.start), 'end':float(r.end), 'local_speaker':int(r.speaker)} for r in results]
        voices = []
        for speaker in sorted({r['local_speaker'] for r in turns}):
            # Avoid using overlapping speech to enroll a voice. At most 15s per profile.
            clean = [r for r in turns if r['local_speaker'] == speaker and not any(
                other['local_speaker'] != speaker and other['start'] < r['end'] and other['end'] > r['start']
                for other in turns)]
            if not clean:
                continue
            parts = [samples[max(0,int(r['start']*16000)):min(len(samples),int(r['end']*16000))] for r in clean]
            clip = np.concatenate(parts)[:15*16000]
            if len(clip) < 2*16000:
                continue
            stream = extractor.create_stream()
            stream.accept_waveform(sample_rate=16000, waveform=clip)
            stream.input_finished()
            if not extractor.is_ready(stream):
                continue
            embedding = np.asarray(extractor.compute(stream), dtype=np.float32)
            norm = float(np.linalg.norm(embedding))
            if not np.isfinite(embedding).all() or norm < 1e-6:
                continue
            voices.append({'local_speaker':speaker, 'embedding':(embedding/norm).tolist(),
                           'ranges':[[r['start'],r['end']] for r in clean]})
        print(json.dumps({'ok':True, 'turns':turns, 'voices':voices}), flush=True)
    except Exception as error:
        print(json.dumps({'ok':False, 'error':str(error)}), flush=True)
