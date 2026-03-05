import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { LicenseKeyRead } from "../models/components/licensekeyread.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { Unauthorized } from "../models/errors/unauthorized.js";
import { LicenseKeysUpdateRequest } from "../models/operations/licensekeysupdate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Update License Key
 *
 * @remarks
 * Update a license key.
 *
 * **Scopes**: `license_keys:write`
 */
export declare function licenseKeysUpdate(client: PolarCore, request: LicenseKeysUpdateRequest, options?: RequestOptions): APIPromise<Result<LicenseKeyRead, Unauthorized | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=licenseKeysUpdate.d.ts.map