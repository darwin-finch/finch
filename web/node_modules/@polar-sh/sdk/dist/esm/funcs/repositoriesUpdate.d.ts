import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Repository } from "../models/components/repository.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { NotPermitted } from "../models/errors/notpermitted.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { RepositoriesUpdateRequest } from "../models/operations/repositoriesupdate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Update Repository
 *
 * @remarks
 * Update a repository.
 *
 * **Scopes**: `repositories:write`
 */
export declare function repositoriesUpdate(client: PolarCore, request: RepositoriesUpdateRequest, options?: RequestOptions): APIPromise<Result<Repository, NotPermitted | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=repositoriesUpdate.d.ts.map