//! Localized visible copy for the native Linux frontend. Keep semantic keys here.
//! Switching locale never changes browsing, selection or playback state.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    Korean,
    English,
}

impl Locale {
    pub fn detect() -> Self {
        // The Korean fork defaults to Korean; an explicit English system locale is respected.
        let language = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|key| std::env::var(key).ok())
            .find(|value| !value.is_empty())
            .unwrap_or_default();
        if language.to_ascii_lowercase().starts_with("en") {
            Self::English
        } else {
            Self::Korean
        }
    }

    pub fn toggle(&mut self) {
        *self = match self {
            Self::Korean => Self::English,
            Self::English => Self::Korean,
        };
    }

    pub fn badge(self) -> &'static str {
        match self {
            Self::Korean => "한국어",
            Self::English => "English",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    Up, Open, AddressHint, SearchScope, SearchHint, Search, BackToFolder,
    HistoryBack, HistoryForward,
    SearchResults, FoldersMedia, EntryLimit, Folder, Audio, MediaDetails,
    SelectionSummary, SelectedCount, Images, Videos, BatchUnavailable,
    GridView, ListView, Fullscreen, ExitFullscreen,
    Compare, LoadingCompare, Comparing,
    EmbeddedDetails, NoDetails, Title, Camera, Lens, Description, Keywords,
    Rating, Rejected, ReadDetailsFailed, PhotoPreview, Pause, Resume, Replay,
    Play, Stop, ZoomOut, ZoomIn, Fit, ActualPixels, ZoomHelp, Previous, Next, OfFit,
    OpeningFolder, EmptyFolder, SelectImage, Indexing, AudioReady,
    LoadingImage, OpeningMedia, FullImageTooLarge, PlaybackFailed,
    PlaybackFinished, Playing, Paused, Stopped, ImagePreview, VideoPreview,
    ImageFull, Matches, SearchFailed, DecodeFailed, FullDecodeFailed,
}

pub fn tr(locale: Locale, key: Text) -> &'static str {
    use Text::*;
    let (ko, en) = match key {
        Up => ("상위 폴더", "Up"),
        HistoryBack => ("이전 폴더", "Back"),
        HistoryForward => ("다음 폴더", "Forward"),
        Open => ("열기", "Open"),
        AddressHint => ("파일 또는 폴더 경로", "File or folder path"),
        SearchScope => ("현재 폴더 및 하위 폴더 검색", "Search this folder and subfolders"),
        SearchHint => ("태그, 텍스트 또는 검색 조건", "Tags, text, or search terms"),
        Search => ("검색", "Search"),
        BackToFolder => ("폴더로 돌아가기", "Back to folder"),
        SearchResults => ("검색 결과", "Search results"),
        FoldersMedia => ("폴더와 미디어", "Folders & media"),
        EntryLimit => ("처음 10,000개만 표시합니다.", "Showing first 10,000 entries."),
        Folder => ("폴더", "Folder"),
        Audio => ("오디오", "Audio"),
        MediaDetails => ("미디어 정보", "Media details"),
        SelectionSummary => ("선택 항목", "Selection"),
        SelectedCount => ("개 선택", "selected"),
        Images => ("이미지", "Images"),
        Videos => ("동영상", "Videos"),
        BatchUnavailable => ("일괄 편집은 아직 지원하지 않습니다.", "Batch editing is not available yet."),
        Compare => ("사진 비교", "Compare photos"),
        LoadingCompare => ("비교할 사진 불러오는 중…", "Loading comparison…"),
        Comparing => ("사진 두 장 비교", "Comparing two photos"),
        Fullscreen => ("전체 화면 (F11)", "Fullscreen (F11)"),
        ExitFullscreen => ("전체 화면 종료 (Esc)", "Exit fullscreen (Esc)"),
        GridView => ("썸네일 격자", "Thumbnail grid"),
        ListView => ("목록", "List"),
        EmbeddedDetails => ("파일에 저장된 메타데이터", "Embedded metadata"),
        NoDetails => ("메타데이터가 없습니다.", "No embedded details found."),
        Title => ("제목", "Title"),
        Camera => ("카메라", "Camera"),
        Lens => ("렌즈", "Lens"),
        Description => ("설명", "Description"),
        Keywords => ("키워드", "Keywords"),
        Rating => ("평점", "Rating"),
        Rejected => ("거부됨", "Rejected"),
        ReadDetailsFailed => ("메타데이터를 읽을 수 없습니다.", "Could not read embedded details."),
        PhotoPreview => ("사진 미리보기", "Photo preview"),
        Pause => ("일시정지", "Pause"),
        Resume => ("계속", "Resume"),
        Replay => ("다시 재생", "Replay"),
        Play => ("재생", "Play"),
        Stop => ("정지", "Stop"),
        ZoomOut => ("축소", "Zoom out"),
        ZoomIn => ("확대", "Zoom in"),
        Fit => ("화면 맞춤", "Fit"),
        ActualPixels => ("원본 픽셀 100%", "Actual pixels 100%"),
        ZoomHelp => ("Ctrl + 휠: 확대·축소 · 드래그: 이동", "Ctrl + scroll to zoom · drag to pan"),
        Previous => ("이전 미디어 (←)", "Previous media (←)"),
        Next => ("다음 미디어 (→)", "Next media (→)"),
        OfFit => ("화면 맞춤 기준", "of fit"),
        OpeningFolder => ("폴더 여는 중…", "Opening folder…"),
        EmptyFolder => ("표시할 폴더나 지원 미디어가 없습니다.", "No folders or supported media files here."),
        SelectImage => ("미디어를 선택하세요.", "Select a media file to preview it."),
        Indexing => ("폴더 색인 및 검색 중…", "Indexing this folder and searching…"),
        AudioReady => ("오디오 준비 완료 · 재생을 누르세요.", "Audio ready · press Play"),
        LoadingImage => ("이미지 불러오는 중…", "Loading image…"),
        OpeningMedia => ("미디어 여는 중…", "Opening media…"),
        FullImageTooLarge => ("원본이 GPU 텍스처 한도를 초과해 미리보기를 유지합니다.", "Full image exceeds the graphics texture limit; showing preview."),
        PlaybackFailed => ("재생 실패", "Playback failed."),
        PlaybackFinished => ("재생 완료", "Playback finished."),
        Playing => ("재생 중", "Playing"),
        Paused => ("일시정지", "Paused"),
        Stopped => ("정지됨", "Stopped"),
        ImagePreview => ("미리보기", "Preview"),
        VideoPreview => ("동영상 미리보기 · 재생을 누르세요", "Video preview · press Play"),
        ImageFull => ("이미지", "Image"),
        Matches => ("개 미디어 일치", "matching media files"),
        SearchFailed => ("검색 실패", "Search failed"),
        DecodeFailed => ("현재 디코더로 이미지를 읽을 수 없습니다.", "Cannot decode this image with the current media backend."),
        FullDecodeFailed => ("원본 해상도 이미지를 읽지 못했습니다.", "Full-resolution image could not be loaded."),
    };
    match locale {
        Locale::Korean => ko,
        Locale::English => en,
    }
}

/// Status is semantic state. Do not cache translated text: language switches are immediate.
pub enum Status {
    Empty,
    Text(Text),
    Image { width: usize, height: usize, video: bool, full: bool },
    Matches(usize),
    SearchError(String),
    Error(String),
}

impl Status {
    pub fn display(&self, locale: Locale) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Text(key) => tr(locale, *key).into(),
            Self::Image { width, height, video, full } => {
                let heading = tr(locale, if *full {
                    Text::ImageFull
                } else if *video {
                    Text::VideoPreview
                } else {
                    Text::ImagePreview
                });
                format!("{heading}: {width} × {height}")
            }
            Self::Matches(count) => {
                if locale == Locale::Korean {
                    format!("{count}{}", tr(locale, Text::Matches))
                } else {
                    format!("{count} {}", tr(locale, Text::Matches))
                }
            }
            Self::SearchError(error) => format!("{}: {error}", tr(locale, Text::SearchFailed)),
            Self::Error(error) => error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn languages_cover_the_same_semantic_keys_and_statuses() {
        let keys = [
            Text::Up, Text::Open, Text::AddressHint, Text::SearchScope,
            Text::SearchHint, Text::Search, Text::BackToFolder,
            Text::HistoryBack, Text::HistoryForward, Text::SearchResults,
            Text::FoldersMedia, Text::EntryLimit, Text::Folder, Text::Audio,
            Text::MediaDetails, Text::SelectionSummary, Text::SelectedCount,
            Text::Images, Text::Videos, Text::BatchUnavailable,
            Text::GridView, Text::ListView, Text::Fullscreen, Text::ExitFullscreen,
            Text::Compare, Text::LoadingCompare, Text::Comparing,
            Text::EmbeddedDetails, Text::NoDetails,
            Text::Title, Text::Camera, Text::Lens, Text::Description,
            Text::Keywords, Text::Rating, Text::Rejected, Text::ReadDetailsFailed,
            Text::PhotoPreview, Text::Pause, Text::Resume, Text::Replay,
            Text::Play, Text::Stop, Text::ZoomOut, Text::ZoomIn, Text::Fit, Text::ActualPixels,
            Text::ZoomHelp, Text::Previous, Text::Next, Text::OfFit,
            Text::OpeningFolder, Text::EmptyFolder, Text::SelectImage,
            Text::Indexing, Text::AudioReady, Text::LoadingImage,
            Text::OpeningMedia, Text::FullImageTooLarge, Text::PlaybackFailed,
            Text::PlaybackFinished, Text::Playing, Text::Paused, Text::Stopped,
            Text::ImagePreview, Text::VideoPreview, Text::ImageFull,
            Text::Matches, Text::SearchFailed, Text::DecodeFailed,
            Text::FullDecodeFailed,
        ];
        for key in keys {
            assert!(!tr(Locale::Korean, key).is_empty());
            assert!(!tr(Locale::English, key).is_empty());
        }
        let mut locale = Locale::Korean;
        locale.toggle();
        assert_eq!(locale, Locale::English);
        assert_eq!(Status::Matches(3).display(Locale::Korean), "3개 미디어 일치");
    }
}
