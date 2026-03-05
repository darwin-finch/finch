import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { LicenseKeyValidate } from "../models/components/licensekeyvalidate.js";
import { ValidatedLicenseKey } from "../models/components/validatedlicensekey.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Validate License Key
 *
 * @remarks
 * Validate a license key.
 */
export declare function customerPortalLicenseKeysValidate(client: PolarCore, request: LicenseKeyValidate, options?: RequestOptions): APIPromise<Result<ValidatedLicenseKey, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalLicenseKeysValidate.d.ts.map