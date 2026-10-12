import concurrent.futures
import json
import pathlib
import urllib.request

root = pathlib.Path('evidence/apple')
root.mkdir(parents=True, exist_ok=True)
paths = [
    'appkit/nsglasseffectview',
    'appkit/nsglasseffectview/tintcolor',
    'appkit/nswindow/backgroundcolor',
    'appkit/nswindow/titlebarappearstransparent',
    'appkit/nstitlebaraccessoryviewcontroller',
    'appkit/nswindowtab/accessoryview',
    'appkit/nstoolbaritem/backgroundtintcolor',
    'appkit/nstoolbaritem/style',
    'swiftui/view',
    'swiftui/glass',
    'swiftui/containerbackgroundplacement/window',
    'swiftui/view/toolbarbackground(_:for:)-5ybst',
    'swiftui/view/toolbarbackground(_:for:)-7lv0f',
    'swiftui/view/toolbarbackgroundvisibility(_:for:)',
    'swiftui/view/containerbackground(_:for:)',
    'swiftui/view/glasseffect(_:in:)',
    'swiftui/glass/tint(_:)',
    'appkit/nsworkspace/accessibilitydisplayoptionsdidchangenotification',
    'usernotifications/unmutablenotificationcontent',
    'appkit/nsdocktile',
    'appkit/nsstatusitem',
]

def fetch(path):
    url = 'https://developer.apple.com/tutorials/data/documentation/' + path + '.json'
    try:
        with urllib.request.urlopen(url, timeout=40) as response:
            data = json.load(response)
        (root / (path.replace('/', '_') + '.json')).write_text(json.dumps(data, indent=2))
        result = {'url': url, 'title': data.get('metadata', {}).get('title'),
                  'platforms': data.get('metadata', {}).get('platforms')}
        result['abstract'] = data.get('abstract')
        if path in ('swiftui/view', 'swiftui/glass'):
            result['matches'] = [{'title': ref.get('title'), 'url': ref.get('url')}
                for ref in data.get('references', {}).values()
                if any(term in ref.get('title', '').lower() for term in
                       ['toolbarbackground', 'containerbackground', 'glasseffect', 'tint('])]
        return result
    except Exception as exc:
        return {'url': url, 'error': str(exc)}

with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
    for result in pool.map(fetch, paths):
        print(json.dumps(result))
