/**
 * kiri の実際の --json の結果（IMG_0251.HEIC を sips で JPEG にし、feat/warning-regions の kiri で
 * `cutout in.jpg -o canvas.png --optimize --canvas 1000x1000 --json` を走らせたもの）から、
 * kiri-review が読まない鍵を落としたもの。パスは in.jpg / canvas.png に置き換えてある
 */
export const CANVAS_REPORT = {
  "schema_version": 2,
  "input": "in.jpg",
  "source": {
    "width": 4284,
    "height": 5712
  },
  "outputs": [
    {
      "path": "canvas.png",
      "format": "png",
      "width": 1000,
      "height": 1000,
      "source_rect": [
        82,
        2073,
        4177,
        3398
      ],
      "content_rect": [
        75,
        362,
        924,
        636
      ]
    }
  ],
  "dry_run": false,
  "warnings": [
    {
      "code": "EDGE_THRESHOLD_RAISED"
    },
    {
      "code": "BACKGROUND_FIELD_USED"
    },
    {
      "code": "LOW_UNIFORMITY"
    },
    {
      "code": "CONTOUR_ROUGH",
      "data": {
        "regions": [
          {
            "bbox": [
              714,
              2078,
              1847,
              2146
            ],
            "normalized_bbox": [
              0.1666,
              0.3637,
              0.4314,
              0.3759
            ],
            "score": 0.5081
          },
          {
            "bbox": [
              1812,
              2101,
              2213,
              2152
            ],
            "normalized_bbox": [
              0.4229,
              0.3678,
              0.5169,
              0.377
            ],
            "score": 0.466
          },
          {
            "bbox": [
              2544,
              2104,
              3677,
              2181
            ],
            "normalized_bbox": [
              0.5938,
              0.3683,
              0.8586,
              0.3821
            ],
            "score": 0.4389
          },
          {
            "bbox": [
              92,
              2062,
              749,
              2213
            ],
            "normalized_bbox": [
              0.0214,
              0.3609,
              0.1751,
              0.3877
            ],
            "score": 0.3548
          }
        ]
      }
    },
    {
      "code": "RIM_CONTAMINATED",
      "data": {
        "regions": [
          {
            "bbox": [
              85,
              2062,
              1115,
              2579
            ],
            "normalized_bbox": [
              0.0198,
              0.3609,
              0.2606,
              0.4517
            ],
            "score": 0.1498
          },
          {
            "bbox": [
              1080,
              2084,
              2213,
              2170
            ],
            "normalized_bbox": [
              0.2521,
              0.3648,
              0.5169,
              0.3801
            ],
            "score": 0.1246
          },
          {
            "bbox": [
              2544,
              2104,
              3677,
              2199
            ],
            "normalized_bbox": [
              0.5938,
              0.3683,
              0.8586,
              0.3852
            ],
            "score": 0.0758
          },
          {
            "bbox": [
              3642,
              2910,
              4186,
              3350
            ],
            "normalized_bbox": [
              0.8501,
              0.5094,
              0.9774,
              0.5867
            ],
            "score": 0.0412
          },
          {
            "bbox": [
              4008,
              2154,
              4170,
              2579
            ],
            "normalized_bbox": [
              0.9355,
              0.3771,
              0.9737,
              0.4517
            ],
            "score": 0.0387
          }
        ]
      }
    }
  ]
}

/** 同じ入力に `--edge-repair auto` を足したもの（警告は解き直しで消え、edge_repair が付く） */
export const REPAIRED_REPORT = {
  "schema_version": 2,
  "input": "in.jpg",
  "source": {
    "width": 4284,
    "height": 5712
  },
  "outputs": [
    {
      "path": "canvas.png",
      "format": "png",
      "width": 1000,
      "height": 1000,
      "source_rect": [
        82,
        2087,
        4170,
        3398
      ],
      "content_rect": [
        75,
        363,
        924,
        635
      ]
    }
  ],
  "dry_run": false,
  "warnings": [
    {
      "code": "EDGE_THRESHOLD_RAISED"
    },
    {
      "code": "BACKGROUND_FIELD_USED"
    },
    {
      "code": "LOW_UNIFORMITY"
    }
  ],
  "edge_repair": {
    "repaired": 9,
    "windows": [
      [
        39,
        2016,
        1161,
        2625
      ],
      [
        46,
        2016,
        795,
        2259
      ],
      [
        668,
        2032,
        1893,
        2192
      ],
      [
        1034,
        2038,
        2259,
        2216
      ],
      [
        1766,
        2055,
        2259,
        2198
      ],
      [
        2498,
        2058,
        3723,
        2227
      ],
      [
        2498,
        2058,
        3723,
        2245
      ],
      [
        3596,
        2864,
        4232,
        3396
      ],
      [
        3962,
        2108,
        4216,
        2625
      ]
    ],
    "gated_by": null,
    "before": {
      "halo_ratio": 0.0239,
      "contour_roughness": 0.2541,
      "rim_contamination": 0.0613
    },
    "after": {
      "halo_ratio": 0.0068,
      "contour_roughness": 0.0939,
      "rim_contamination": 0.0111
    }
  }
}
