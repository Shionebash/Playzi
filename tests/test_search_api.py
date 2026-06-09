from __future__ import annotations

import unittest
from unittest.mock import MagicMock, patch

from fastapi.testclient import TestClient

from backend.main import app


class SearchTests(unittest.TestCase):
    def test_youtube_search_cache_includes_limit(self) -> None:
        from backend import youtube

        youtube._SEARCH_CACHE.clear()
        seen_targets: list[str] = []

        class FakeYDL:
            def __init__(self, opts):
                self.opts = opts

            def __enter__(self):
                return self

            def __exit__(self, exc_type, exc, tb):
                return False

            def extract_info(self, target, download=False):
                seen_targets.append(target)
                count = int(target.split(":", 1)[0].replace("ytsearch", ""))
                return {"entries": [{"id": f"video{i:02d}", "title": f"Song {i}", "webpage_url": f"https://www.youtube.com/watch?v=video{i:02d}"} for i in range(count)]}

        with patch("backend.youtube.yt_dlp.YoutubeDL", FakeYDL):
            small = youtube.search("demo artist", limit=2)
            large = youtube.search("demo artist", limit=4)

        self.assertEqual(len(small), 2)
        self.assertEqual(len(large), 4)
        self.assertEqual(seen_targets, ["ytsearch2:demo artist", "ytsearch4:demo artist"])

    def test_api_search_all_uses_separate_explicit_limits(self) -> None:
        client = TestClient(app)
        with (
            patch("backend.main.config.YTDLP_SEARCH_LIMIT", 12),
            patch("backend.main.search", return_value=[{"title": "YT"}]) as yt_search,
            patch("backend.main.search_music", return_value=[{"title": "Music"}]) as music_search,
        ):
            response = client.get("/api/search", params={"q": "demo artist", "scope": "all"})

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["items"], [{"title": "YT"}, {"title": "Music"}])
        yt_search.assert_called_once_with("demo artist", 12)
        music_search.assert_called_once_with("demo artist", limit=30)

    def test_music_search_maps_artist_as_browse_result(self) -> None:
        from backend import ytmusic

        ytmusic._MUSIC_SEARCH_CACHE.clear()
        fake_client = MagicMock()
        fake_client.search.return_value = [
            {
                "resultType": "artist",
                "browseId": "UCartist",
                "title": "Demo Artist",
                "category": "Artists",
                "thumbnails": [{"url": "https://thumb/artist.jpg"}],
            }
        ]

        with patch("backend.ytmusic._ytmusic_client", return_value=fake_client):
            items = ytmusic.search_music("demo artist", limit=30, search_filter="artists")

        self.assertEqual(items[0]["resultType"], "artist")
        self.assertEqual(items[0]["url"], "https://music.youtube.com/browse/UCartist")
        self.assertNotIn("playlist?list=", items[0]["url"])

    def test_music_search_keeps_tracks_playable(self) -> None:
        from backend import ytmusic

        ytmusic._MUSIC_SEARCH_CACHE.clear()
        fake_client = MagicMock()
        fake_client.search.return_value = [
            {
                "resultType": "song",
                "videoId": "abc123xyz00",
                "title": "Demo Song",
                "duration": "3:21",
                "artists": [{"name": "Demo Artist"}],
                "thumbnails": [{"url": "https://thumb/song.jpg"}],
            }
        ]

        with patch("backend.ytmusic._ytmusic_client", return_value=fake_client):
            items = ytmusic.search_music("demo artist", limit=30, search_filter="songs")

        self.assertEqual(items[0]["resultType"], "track")
        self.assertEqual(items[0]["url"], "https://music.youtube.com/watch?v=abc123xyz00")
        self.assertEqual(items[0]["channel"], "Demo Artist")


class AuthRefreshTests(unittest.TestCase):
    def test_silent_auth_refresh_returns_state_and_account(self) -> None:
        from backend import main

        fake_client = MagicMock()
        with patch("backend.ytmusic._get_validated_client", return_value=(fake_client, "ok", "Demo User")):
            result = main._run_silent_auth_refresh()

        self.assertEqual(result, {"authState": "ok", "accountName": "Demo User"})

    def test_silent_auth_refresh_expired_clears_account(self) -> None:
        from backend import main

        fake_client = MagicMock()
        with patch("backend.ytmusic._get_validated_client", return_value=(fake_client, "expired", "")):
            result = main._run_silent_auth_refresh()

        self.assertEqual(result, {"authState": "expired", "accountName": ""})

    def test_auth_refresh_endpoint_returns_task(self) -> None:
        client = TestClient(app)
        # Patch start_task so no real worker escapes (would spawn Playwright and
        # mutate the live data/state.json on installs that have the deps).
        with patch("backend.main.start_task", return_value={"id": "fake-task"}) as start:
            response = client.post("/api/music/auth/refresh")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["task"], {"id": "fake-task"})
        self.assertEqual(start.call_args.args[0], "auth-refresh")


if __name__ == "__main__":
    unittest.main()
